//! The effect runner. Every effect that answers is a future that the view
//! runs as a task node: HTTP awaits an ephemeral `NSURLSession`, and key
//! derivation awaits [`off_thread`].
//!
//! Storage keeps nothing yet: every slot write succeeds and is dropped, and
//! a start finds every slot empty. The slots hold the refresh token and the
//! vault, which must go in the Keychain, and plain files would leave them
//! readable on disk.

mod http;
mod settings;

use appkit_executor::off_thread;
use chaveiro_core::{
    core::{DeviceKeyError, Effect, Input},
    crypto::derive_master_key,
};
use objc2::rc::Retained;
use objc2_foundation::NSURLSession;
pub use settings::{Settings, SettingsError};
use std::{future::Future, path::PathBuf, pin::Pin};

/// Holds the effects of the core as futures for the view to run. Answers
/// may come in any order.
pub struct Io {
    session: Retained<NSURLSession>,
    /// `~/Library/Application Support/Chaveiro`, or `CHAVEIRO_DATA_DIR`
    /// when set.
    folder: PathBuf,
    /// Effects that run as task nodes of the view, until their answer is
    /// taken. The future is [`None`] once a pass started it.
    pending: Vec<(TaskId, Option<Work>)>,
    next_task: u64,
}

/// Names one task of [`Io`], so its answer can end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

/// The future of one effect that runs as a task node. It yields [`None`]
/// when the effect has no answer, such as a derivation that panicked.
pub type Work = Pin<Box<dyn Future<Output = Option<Input>>>>;

impl Io {
    pub fn new() -> Self {
        let folder = match std::env::var_os("CHAVEIRO_DATA_DIR") {
            Some(folder) => PathBuf::from(folder),
            None => {
                let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
                PathBuf::from(home).join("Library/Application Support/Chaveiro")
            }
        };
        Io {
            session: http::session(),
            folder,
            pending: Vec::new(),
            next_task: 0,
        }
    }

    /// Reads the settings file. [`None`] when there is none yet.
    pub fn load_settings(&self) -> Result<Option<Settings>, SettingsError> {
        settings::read(&self.folder.join(settings::FILE))
    }

    /// Writes the settings file. A failed write is only logged.
    pub fn save_settings(&self, settings: &Settings) {
        if let Err(error) = settings::write(&self.folder.join(settings::FILE), settings) {
            tracing::warn!(%error, "saving settings failed");
        }
    }

    /// Adds `effect`. Its future, if it has one, waits in
    /// [`pending`](Self::pending) for the view to run it.
    pub fn run(&mut self, effect: Effect) {
        match effect {
            Effect::Http { id, request } => {
                let response = http::send(&self.session, request);
                self.pend(async move {
                    Some(Input::Http {
                        id,
                        response: response.await,
                    })
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
                // A panic is on stderr, and the core keeps waiting, as it
                // does for a lost request.
                result.map(|result| Input::Derived { id, result })
            }),
            // Dropped until the Keychain store lands; see the module doc.
            Effect::Store { id, .. } => {
                self.pend(async move { Some(Input::Stored { id, result: Ok(()) }) });
            }
            Effect::Clear { .. } => {}
            // A pass runs after each answer, so there is nothing to do.
            Effect::Rerender => {}
            // Dropped until the vault panes offer copy, because nothing here
            // sends the copy action yet.
            Effect::Copy { .. } => {}
            Effect::DeviceSecret { id, .. } => {
                // Touch ID lands with the lock screen.
                self.pend(async move {
                    Some(Input::DeviceSecret {
                        id,
                        result: Err(DeviceKeyError::Unavailable),
                    })
                });
            }
            Effect::PasskeyReply { .. } | Effect::PasskeyCatalog { .. } => {}
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

    /// Forgets every pending effect. The view then declares no task, so the
    /// next pass drops the futures, which cancels their requests. A key
    /// derivation already on the pool runs to its end, and its result drops
    /// there. Call it when the app closes.
    pub fn close(&mut self) {
        self.pending.clear();
    }

    /// Adds `future` as a pending effect for the view to run.
    fn pend(&mut self, future: impl 'static + Future<Output = Option<Input>>) {
        let task = TaskId(self.next_task);
        self.next_task += 1;
        self.pending.push((task, Some(Box::pin(future))));
    }
}
