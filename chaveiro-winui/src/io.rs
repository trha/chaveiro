//! The effect runner. Every effect is a future that the view runs as a
//! task node, and CPU-bound or blocking work inside it runs on the process
//! thread pool through [`off_thread`].

pub(crate) mod clipboard;
mod device_key;
mod http;
pub(crate) mod key_file;
mod storage;

use crate::bindings::HttpClient;
use chaveiro_core::{
    core::{Effect, Input, StorageError, StoredAccount},
    crypto::derive_master_key,
};
use std::{
    cell::RefCell,
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
};
pub use storage::{Settings, SettingsError};
use windows_future::IAsyncAction;

use zeroize::Zeroizing;

/// Holds the effects of the core as futures for the view to run, and the
/// answers of file work until the view takes them.
///
/// HTTP, key derivation and Windows Hello may answer in any order. File work
/// runs one job at a time in the order it was asked for, so two writes of
/// one slot never race and a boot read sees every earlier write.
pub struct Io {
    client: HttpClient,
    /// `%LOCALAPPDATA%\chaveiro`, or `CHAVEIRO_DATA_DIR` when set.
    folder: PathBuf,
    /// The folder of the account's storage slots, inside `folder`.
    account: Arc<Path>,
    /// The main window's `AppWindow.Id`, which owns the Hello prompt.
    /// [`None`] before the first pass.
    window: Option<u64>,
    /// Effects that run as task nodes of the view, until their answer is
    /// taken. The future is [`None`] once a pass started it.
    pending: Vec<(TaskId, Option<Work>)>,
    next_task: u64,
    storage: Storage,

    /// The stream of file work answers until the view starts it.
    stored: Option<StoredAnswers>,
}


/// Names one task of [`Io`], so its answer can end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(u64);



/// The future of one effect that runs as a task node.
pub type Work = std::pin::Pin<Box<dyn Future<Output = Answer>>>;

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
    pub fn new() -> windows_core::Result<Self> {
        let folder = match std::env::var_os("CHAVEIRO_DATA_DIR") {
            Some(folder) => PathBuf::from(folder),
            None => {
                let local = std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into());
                PathBuf::from(local).join("chaveiro")
            }
        };
        let (storage, stored) = Storage::new();
        let io = Io {
            client: http::client()?,
            storage,
            account: folder.join("account").into(),
            folder,
            window: None,
            pending: Vec::new(),
            next_task: 0,

            stored: Some(stored),
        };
        Ok(io)
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
                self.pend(async move {
                    let response = http::send(&client, request).await;
                    Answer::Core(Input::Http { id, response })
                });
            }
            Effect::Derive {
                id,
                password,
                email,
                kdf,
            } => self.pend(async move {
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
            // A copy is UI: the app's `Copies` runs it as a flow.
            Effect::Copy { .. } => unreachable!("the app routes copies to `Copies`"),
            Effect::DeviceSecret {
                id,
                challenge,
                create,
            } => {
                // A pending effect runs until it answers, so a lock does not
                // end the Hello prompt, and the core drops a late answer.
                // Cancelling would not close the prompt anyway, because
                // Windows keeps it open after `Cancel`.
                let window = self.window;
                self.pend(async move {
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

    /// The effects that run as task nodes, for the view to declare. It
    /// declares each under its [`TaskId`] on every pass, starting the work
    /// with [`Option::take`] on the first one, until it calls
    /// [`answered`](Self::answered).
    pub fn pending(&mut self) -> &mut [(TaskId, Option<Work>)] {
        &mut self.pending
    }

    /// Whether an effect waits for its first pass, which the view must then
    /// ask for.
    pub fn unstarted(&self) -> bool {
        self.pending.iter().any(|(_, work)| work.is_some())
    }

    /// Ends the pending effect `task`, whose answer the view took.
    pub fn answered(&mut self, task: TaskId) {
        self.pending.retain(|(id, _)| *id != task);
    }

    /// The stream of file work answers, for the view to run as a stream
    /// node from its first pass. Panics when called twice.
    pub fn stored_answers(&mut self) -> StoredAnswers {
        self.stored.take().expect("the view starts the storage stream once")
    }

    /// Forgets every pending effect and runs the queued file work on this
    /// thread, after the job already running ends. Call it when the app
    /// closes, so no write is lost. The view then declares no task, so the
    /// next pass drops the futures, which aborts them.
    pub fn close(&mut self) {
        self.pending.clear();
        self.storage.flush();
    }

    /// The window that owns the Hello prompt and the clipboard while a
    /// copy lands; see [`set_window`](Self::set_window).
    pub fn window(&self) -> Option<u64> {
        self.window
    }

    /// Adds `future` as a pending effect for the view to run.
    fn pend(&mut self, future: impl 'static + Future<Output = Answer>) {
        let task = TaskId(self.next_task);
        self.next_task += 1;
        self.pending.push((task, Some(Box::pin(future))));
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
/// result is dropped on the pool thread. `winui_executor::shutdown` waits
/// for it.
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
    // The closure always returns Ok, and a spawned action ignores a cancel,
    // so the await cannot fail.
    winui_executor::winrt(action)
        .await
        .expect("an action that returns Ok completes");
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
}

/// The answers of [`Storage`]'s jobs, as a stream the view runs as a
/// stream node. It runs one job at a time on the pool, in push order, and
/// yields the answer of each job that has one. It never ends.
pub struct StoredAnswers {
    queue: Rc<RefCell<Queue>>,
    running: Arc<AtomicBool>,
    /// The UI thread, the one `flush` parks, which each job unparks.
    ui: std::thread::Thread,
    /// The job on the pool.
    job: Option<std::pin::Pin<Box<dyn Future<Output = Option<Answer>>>>>,
}

impl futures_core::Stream for StoredAnswers {
    type Item = Answer;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> Poll<Option<Answer>> {
        loop {
            if let Some(job) = self.job.as_mut() {
                let Poll::Ready(answer) = job.as_mut().poll(context) else {
                    return Poll::Pending;
                };
                self.job = None;
                if let Some(answer @ (Answer::Core(_) | Answer::Settings(_))) = answer {
                    return Poll::Ready(Some(answer));
                }
                continue;
            }
            let job = {
                let mut queue = self.queue.borrow_mut();
                match queue.jobs.pop_front() {
                    Some(job) => job,
                    None => {
                        queue.waker = Some(context.waker().clone());
                        return Poll::Pending;
                    }
                }
            };
            // Relaxed: only this thread reads the flag before the job
            // clears it.
            self.running.store(true, Ordering::Relaxed);
            let running = Arc::clone(&self.running);
            let ui = self.ui.clone();
            self.job = Some(Box::pin(off_thread(move || {
                let answer = catch_unwind(AssertUnwindSafe(job));
                // Release: `flush` sees the job's writes done once it sees
                // false.
                running.store(false, Ordering::Release);
                ui.unpark();
                answer.unwrap_or(Answer::None)
            })));
        }
    }
}

type Job = Box<dyn 'static + Send + FnOnce() -> Answer>;

struct Queue {
    jobs: VecDeque<Job>,
    /// The waker of the answers stream while it waits for a job.
    waker: Option<Waker>,
}

impl Storage {
    /// The storage, and the stream that runs its queue.
    fn new() -> (Self, StoredAnswers) {
        let queue = Rc::new(RefCell::new(Queue {
            jobs: VecDeque::new(),
            waker: None,
        }));
        let running = Arc::new(AtomicBool::new(false));
        let answers = StoredAnswers {
            queue: Rc::clone(&queue),
            running: Arc::clone(&running),
            // `new` runs on the UI thread, the one `flush` parks.
            ui: std::thread::current(),
            job: None,
        };
        (Storage { queue, running }, answers)
    }

    fn push(&mut self, job: impl 'static + Send + FnOnce() -> Answer) {
        let mut queue = self.queue.borrow_mut();
        queue.jobs.push_back(Box::new(job));
        if let Some(waker) = queue.waker.take() {
            waker.wake();
        }
    }

    /// Runs the jobs left on this thread, after the job already on the
    /// pool. Their answers are dropped, because the app is closing. The
    /// stream finds the queue empty and stays idle until it drops.
    fn flush(&mut self) {
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
