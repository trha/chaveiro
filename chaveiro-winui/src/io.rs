//! The effect runner. Every effect is a task on the UI thread's
//! `DispatcherQueue` that [`Io`] owns, and CPU-bound or blocking work inside
//! a task runs on the process thread pool through [`off_thread`].

mod clipboard;
mod device_key;
mod http;
mod storage;

use crate::{Message, bindings::HttpClient};
use chaveiro_core::{
    core::{Effect, Input, StorageError, StoredAccount},
    crypto::derive_master_key,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    time::Duration,
};
pub use storage::{Settings, SettingsError};
use windows_future::IAsyncAction;
use winui_executor::SpawnHandle;
use winui_immediate::Handle;
use zeroize::Zeroizing;

/// Runs the effects of the core and answers each through the app's
/// [`Handle`] with a [`Message::Io`].
///
/// HTTP, key derivation and Windows Hello may answer in any order. File work
/// runs one job at a time in the order it was asked for, so two writes of
/// one slot never race and a boot read sees every earlier write.
pub struct Io {
    handle: Handle<Message>,
    client: HttpClient,
    /// `%LOCALAPPDATA%\chaveiro`, or `CHAVEIRO_DATA_DIR` when set.
    folder: PathBuf,
    /// The folder of the account's storage slots, inside `folder`.
    account: Arc<Path>,
    /// The main window's `AppWindow.Id`, which owns the Hello prompt.
    /// [`None`] before the first pass.
    window: Option<u64>,
    /// One entry per running effect. Dropping an entry aborts its task.
    tasks: Vec<(TaskId, SpawnHandle)>,
    next_task: u64,
    storage: Storage,
    /// The last copy to the clipboard, until it is cleared.
    /// The last copy, until it is cleared. The copy task writes it.
    copied: Rc<Cell<Option<clipboard::Copied>>>,
    /// The task of the last copy: its retries, then its clear.
    copy: Option<SpawnHandle>,
}

/// Names one task of [`Io`], so its answer can end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId(u64);

/// How long a copy stays on the clipboard.
const CLEAR_CLIPBOARD_AFTER: Duration = Duration::from_secs(30);

/// What a task of [`Io`] sends back.
pub enum Answer {
    Core(Input),
    /// [`None`] when there is no settings file yet.
    Settings(Result<Option<Settings>, SettingsError>),
    /// The job had no answer, or it panicked and its message is on stderr.
    None,
}

impl Io {
    /// Panics if this thread has no `DispatcherQueue`, so call it from the
    /// app's start.
    pub fn new(handle: Handle<Message>) -> windows_core::Result<Self> {
        let folder = match std::env::var_os("CHAVEIRO_DATA_DIR") {
            Some(folder) => PathBuf::from(folder),
            None => {
                let local = std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into());
                PathBuf::from(local).join("chaveiro")
            }
        };
        Ok(Io {
            client: http::client()?,
            storage: Storage::new(handle.clone()),
            handle,
            account: folder.join("account").into(),
            folder,
            window: None,
            tasks: Vec::new(),
            next_task: 0,
            copied: Rc::default(),
            copy: None,
        })
    }

    /// Sets the window that owns the Hello prompt: the one the last pass
    /// showed.
    pub fn set_window(&mut self, window: u64) {
        self.window = Some(window);
    }

    /// Reads the settings file and answers with [`Answer::Settings`].
    pub fn load_settings(&mut self) {
        let path = self.folder.join(storage::SETTINGS);
        self.storage
            .push(move || Answer::Settings(storage::read_settings(&path)));
    }

    /// Writes the settings file. A failed write is only logged.
    pub fn save_settings(&mut self, settings: &Settings) {
        let bytes = serde_json::to_vec_pretty(settings).expect("settings always serialize");
        let path = self.folder.join(storage::SETTINGS);
        self.storage.push(move || {
            if let Err(error) = storage::write_atomic(&path, &bytes) {
                tracing::warn!(%error, "saving settings failed");
            }
            Answer::None
        });
    }

    /// Reads every slot and answers with [`Input::Started`]. A slot that
    /// fails to read or unprotect counts as empty.
    pub fn boot(&mut self) {
        let folder = Arc::clone(&self.account);
        self.storage.push(move || {
            Answer::Core(Input::Started {
                stored: StoredAccount {
                    account: storage::load(&folder, chaveiro_core::core::StoreSlot::Account),
                    refresh_token: storage::load(
                        &folder,
                        chaveiro_core::core::StoreSlot::RefreshToken,
                    ),
                    vault_cache: storage::load(&folder, chaveiro_core::core::StoreSlot::VaultCache),
                    device_wrap: storage::load(&folder, chaveiro_core::core::StoreSlot::DeviceWrap),
                    two_factor_token: storage::load(
                        &folder,
                        chaveiro_core::core::StoreSlot::TwoFactorToken,
                    ),
                },
            })
        });
    }

    /// Starts `effect`. Its answer, if it has one, arrives later as a
    /// [`Message::Io`].
    pub fn run(&mut self, effect: Effect) {
        let folder = &self.account;
        match effect {
            Effect::Http { id, request } => {
                let client = self.client.clone();
                self.spawn(async move {
                    let response = http::send(&client, request).await;
                    Answer::Core(Input::Http { id, response })
                });
            }
            Effect::Derive {
                id,
                password,
                email,
                kdf,
            } => self.spawn(async move {
                let result = off_thread(move || {
                    let result = derive_master_key(&password, &email, kdf);
                    drop(password);
                    result
                })
                .await;
                match result {
                    Some(result) => Answer::Core(Input::Derived { id, result }),
                    None => Answer::None,
                }
            }),
            Effect::Store { id, slot, bytes } => {
                let folder = Arc::clone(folder);
                self.storage.push(move || {
                    let bytes = Zeroizing::new(bytes);
                    let result =
                        storage::store(&folder, slot, &bytes).map_err(|error| StorageError {
                            message: error.to_string(),
                        });
                    Answer::Core(Input::Stored { id, result })
                });
            }
            Effect::Clear { slot } => {
                let path = folder.join(storage::file_name(slot));
                self.storage.push(move || {
                    match std::fs::remove_file(&path) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            tracing::warn!(%error, path = %path.display(), "clearing a slot failed");
                        }
                        _ => {}
                    }
                    Answer::None
                });
            }
            // Every pass reads the screen again, and a pass runs after each
            // message, so there is nothing to do.
            Effect::Rerender => {}
            // A later copy replaces this one, aborting its retries and its
            // clear, because that copy's own clear runs.
            Effect::Copy { text } => {
                let last = Rc::clone(&self.copied);
                let window = self.window;
                self.copy = Some(winui_executor::spawn(async move {
                    let mut attempts = 0;
                    let copied = loop {
                        match clipboard::copy(window, &text) {
                            Ok(copied) => break copied,
                            // Another app holds the clipboard for a moment,
                            // as monitors do after each change.
                            Err(clipboard::CopyError::Busy) if attempts < 5 => {
                                attempts += 1;
                                winui_executor::sleep(Duration::from_millis(20)).await;
                            }
                            Err(error) => {
                                tracing::warn!(?error, "nothing was copied");
                                return;
                            }
                        }
                    };
                    drop(text);
                    last.set(Some(copied));
                    winui_executor::sleep(CLEAR_CLIPBOARD_AFTER).await;
                    clipboard::clear(copied);
                    last.set(None);
                }));
            }
            Effect::DeviceSecret {
                id,
                challenge,
                create,
            } => {
                let window = self.window;
                self.spawn(async move {
                    let result = device_key::secret(window, challenge, create).await;
                    Answer::Core(Input::DeviceSecret { id, result })
                });
            }
            // The passkey provider lands with plan WP-08.
            Effect::PasskeyReply { .. } | Effect::PasskeyCatalog { .. } => {}
            // The SSH agent pipe lands with plan WP-10.
            Effect::SshAgentReply { .. } => {}
        }
    }

    /// Ends the entry of a task whose answer arrived.
    pub fn finish(&mut self, task: TaskId) {
        if let Some(index) = self.tasks.iter().position(|(id, _)| *id == task) {
            // The task already sent its answer, so the drop aborts nothing.
            drop(self.tasks.swap_remove(index));
        }
    }

    /// Aborts every running effect and runs the queued file work on this
    /// thread, after the job already running ends. Call it when the app
    /// closes, so no write is lost.
    pub fn close(&mut self) {
        self.tasks.clear();
        self.clear_clipboard();
        self.storage.flush();
    }

    /// Empties the clipboard if it still holds the last copy, and stops a
    /// copy still trying. Call it when the vault locks.
    pub fn clear_clipboard(&mut self) {
        self.copy = None;
        if let Some(copied) = self.copied.take() {
            clipboard::clear(copied);
        }
    }

    fn spawn(&mut self, future: impl 'static + Future<Output = Answer>) {
        let task = TaskId(self.next_task);
        self.next_task += 1;
        let handle = self.handle.clone();
        let spawned = winui_executor::spawn(async move {
            let answer = future.await;
            handle.send(Message::Io {
                task: Some(task),
                answer,
            });
        });
        self.tasks.push((task, spawned));
    }
}

/// Opens `url` in the default browser. The caller passes only `http` and
/// `https` addresses, because the shell opens whatever program a scheme
/// names. A failure only logs: the user sees nothing open and can try
/// again. It returns at once: the shell runs on a thread of its own.
pub fn open_website(url: &url::Url) {
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, target) = (wide("open"), wide(url.as_str()));
    // ShellExecuteW pumps messages while it waits, which on the UI thread
    // runs the next pass inside this one. The shell asks for a COM
    // apartment of its own, so the thread is new and not one of the pool.
    let spawned = std::thread::Builder::new()
        .name("open website".into())
        .spawn(move || shell_open(&verb, &target));
    if let Err(error) = spawned {
        tracing::warn!(%error, "the browser did not open");
    }
}

/// Runs `ShellExecuteW` with `verb` on `target`, both NUL-terminated, in a
/// single-threaded COM apartment, as the shell asks.
fn shell_open(verb: &[u16], target: &[u16]) {
    use windows_sys::Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    // SAFETY: a new thread, so no apartment is set yet.
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    } >= 0;
    // SAFETY: both strings end in a NUL and outlive the call; the null
    // window, parameters and directory are allowed.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // A value of 32 or less is an error code.
    if result as usize <= 32 {
        tracing::warn!(code = result as usize, "the browser did not open");
    }
    if initialized {
        // SAFETY: matches the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

/// Runs `job` on the process thread pool and returns its result, or
/// [`None`] when it panicked (the panic message is then on stderr).
///
/// Dropping the future does not stop the job: it runs to the end and its
/// result is dropped on the pool thread.
pub async fn off_thread<T: 'static + Send>(job: impl 'static + Send + FnOnce() -> T) -> Option<T> {
    let slot = Arc::new(Mutex::new(None));
    let result = Arc::clone(&slot);
    // A panic must not unwind into the thread pool's callback, which aborts
    // the process.
    let action = IAsyncAction::spawn(move || {
        let value = catch_unwind(AssertUnwindSafe(job)).ok();
        *result.lock().expect("the slot is never locked twice") = value;
        Ok(())
    });
    // The closure always returns Ok and nothing cancels the action, so the
    // await cannot fail.
    action.await.expect("an action that returns Ok completes");
    slot.lock()
        .expect("the job ended, so it holds no lock")
        .take()
}

/// File work, one job at a time, in push order.
struct Storage {
    queue: Rc<RefCell<Queue>>,
    /// True from the moment a job is handed to the pool until it ends, so
    /// [`flush`](Self::flush) can wait for it. The job unparks the UI thread
    /// when it clears this.
    running: Arc<AtomicBool>,
    /// The task that runs the queue. Dropping it stops the queue.
    task: Option<SpawnHandle>,
}

type Job = Box<dyn 'static + Send + FnOnce() -> Answer>;

struct Queue {
    jobs: VecDeque<Job>,
    /// The waker of the queue's task while it waits for a job.
    waker: Option<Waker>,
}

impl Storage {
    fn new(handle: Handle<Message>) -> Self {
        let queue = Rc::new(RefCell::new(Queue {
            jobs: VecDeque::new(),
            waker: None,
        }));
        let running = Arc::new(AtomicBool::new(false));
        // `new` runs on the UI thread, the one `flush` parks.
        let ui = std::thread::current();
        let task = winui_executor::spawn({
            let queue = Rc::clone(&queue);
            let running = Arc::clone(&running);
            async move {
                loop {
                    let job = std::future::poll_fn(|context| {
                        let mut queue = queue.borrow_mut();
                        match queue.jobs.pop_front() {
                            Some(job) => Poll::Ready(job),
                            None => {
                                queue.waker = Some(context.waker().clone());
                                Poll::Pending
                            }
                        }
                    })
                    .await;

                    // Relaxed: only this thread reads the flag before the job
                    // clears it.
                    running.store(true, Ordering::Relaxed);
                    let answer = off_thread({
                        let running = Arc::clone(&running);
                        let ui = ui.clone();
                        move || {
                            let answer = catch_unwind(AssertUnwindSafe(job));
                            // Release: `flush` sees the job's writes done once it
                            // sees false.
                            running.store(false, Ordering::Release);
                            ui.unpark();
                            answer.unwrap_or(Answer::None)
                        }
                    })
                    .await;
                    if let Some(answer @ (Answer::Core(_) | Answer::Settings(_))) = answer {
                        handle.send(Message::Io { task: None, answer });
                    }
                }
            }
        });
        Storage {
            queue,
            running,
            task: Some(task),
        }
    }

    fn push(&mut self, job: impl 'static + Send + FnOnce() -> Answer) {
        let mut queue = self.queue.borrow_mut();
        queue.jobs.push_back(Box::new(job));
        if let Some(waker) = queue.waker.take() {
            waker.wake();
        }
    }

    /// Stops the queue's task and runs the jobs left on this thread. Their
    /// answers are dropped, because the app is closing.
    fn flush(&mut self) {
        drop(self.task.take());
        // Waits for the job already on the pool, so the jobs left run after it.
        // An unpark before the park leaves a token, so no wakeup is lost; a
        // spurious wakeup only checks the flag again.
        while self.running.load(Ordering::Acquire) {
            std::thread::park();
        }
        let jobs = std::mem::take(&mut self.queue.borrow_mut().jobs);
        for job in jobs {
            // A panicking job loses only its own work; the message is on
            // stderr.
            let _ = catch_unwind(AssertUnwindSafe(job));
        }
    }
}
