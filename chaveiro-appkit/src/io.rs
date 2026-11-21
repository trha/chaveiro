//! The effect runner. Every effect that answers later is a task on the main
//! queue that [`Io`] owns: HTTP awaits an ephemeral `NSURLSession`, and key
//! derivation awaits [`off_thread`]. The answers come back through the app's
//! [`Handle`] as [`Message::Io`].
//!
//! Storage keeps nothing yet: every slot write succeeds and is dropped, and
//! a start finds every slot empty. The slots hold the refresh token and the
//! vault, which must go in the Keychain, and plain files would leave them
//! readable on disk.

mod http;
mod settings;

use crate::Message;
use appkit_executor::{SpawnHandle, off_thread, spawn};
use appkit_immediate::Handle;
use chaveiro_core::{
    core::{DeviceKeyError, Effect, Input},
    crypto::derive_master_key,
};
use objc2::rc::Retained;
use objc2_foundation::NSURLSession;
pub use settings::{Settings, SettingsError};
use std::path::PathBuf;

/// Runs the effects of the core and answers each through the app's
/// [`Handle`] with a [`Message::Io`]. Answers may come in any order.
pub struct Io {
    handle: Handle<Message>,
    session: Retained<NSURLSession>,
    /// `~/Library/Application Support/Chaveiro`, or `CHAVEIRO_DATA_DIR`
    /// when set.
    folder: PathBuf,
    /// One entry per running effect. Dropping an entry aborts its task,
    /// which cancels its request.
    tasks: Vec<(TaskId, SpawnHandle)>,
    next_task: u64,
}

/// Names one task of [`Io`], so its answer can end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId(u64);

impl Io {
    pub fn new(handle: Handle<Message>) -> Self {
        let folder = match std::env::var_os("CHAVEIRO_DATA_DIR") {
            Some(folder) => PathBuf::from(folder),
            None => {
                let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
                PathBuf::from(home).join("Library/Application Support/Chaveiro")
            }
        };
        Io {
            handle,
            session: http::session(),
            folder,
            tasks: Vec::new(),
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

    /// Starts `effect`. Its answer, if it has one, arrives later as a
    /// [`Message::Io`].
    pub fn run(&mut self, effect: Effect) {
        match effect {
            Effect::Http { id, request } => {
                let response = http::send(&self.session, request);
                self.spawn(async move {
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
            } => self.spawn(async move {
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
                // The core waits for this answer, and the app is running.
                let _ = self.handle.send(Message::Io {
                    task: None,
                    input: Input::Stored { id, result: Ok(()) },
                });
            }
            Effect::Clear { .. } => {}
            // A pass runs after each message, so there is nothing to do.
            Effect::Rerender => {}
            Effect::DeviceSecret { id, .. } => {
                // Touch ID lands with the lock screen.
                let _ = self.handle.send(Message::Io {
                    task: None,
                    input: Input::DeviceSecret {
                        id,
                        result: Err(DeviceKeyError::Unavailable),
                    },
                });
            }
            Effect::PasskeyReply { .. } | Effect::PasskeyCatalog { .. } => {}
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

    /// Aborts every running effect, which cancels its request. A key
    /// derivation already on the pool runs to its end, and its result drops
    /// there. Call it when the app closes.
    pub fn close(&mut self) {
        self.tasks.clear();
    }

    /// Spawns `future` as a task of this `Io`, which sends its input, if
    /// any, as a [`Message::Io`].
    fn spawn(&mut self, future: impl 'static + Future<Output = Option<Input>>) {
        let task = TaskId(self.next_task);
        self.next_task += 1;
        let handle = self.handle.clone();
        let spawned = spawn(async move {
            if let Some(input) = future.await {
                // After close the app no longer waits for answers.
                let _ = handle.send(Message::Io {
                    task: Some(task),
                    input,
                });
            }
        });
        self.tasks.push((task, spawned));
    }
}
