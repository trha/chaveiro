//! The clipboard copy as a flow: copy with retries, show the countdown,
//! then clear. It decides what the user sees and when, so it is UI code;
//! the clipboard calls it makes live in `io::clipboard`. The vault example
//! includes this file too, so its UI script drives the same code the app
//! runs.

use crate::clipboard::{self, Copied, CopyError};
use chaveiro_core::crypto::SecretString;
use std::time::{Duration, Instant};
use winui_immediate::{Align, Ui, owned};

/// How long a copy stays on the clipboard.
pub const CLEAR_CLIPBOARD_AFTER: Duration = Duration::from_secs(30);

/// The last copy asked for, and the clipboard it left.
///
/// Contract: `copied` names the copy on the clipboard from the poll it
/// lands in until its flow clears it, or until [`clear`](Self::clear), and
/// only the newest copy's flow writes it, through the value the flow lends.
#[derive(Default)]
pub struct Copies {
    /// The newest copy, until its flow ends. [`status`](Self::status) runs
    /// it.
    request: Option<CopyRequest>,
    /// Names each copy's flow.
    next: u64,
    copied: Option<Copied>,
}

struct CopyRequest {
    id: u64,
    /// Taken when the flow starts.
    text: Option<SecretString>,
}

impl Copies {
    /// Asks for `text` to be copied. A copy still running is replaced: the
    /// new id names a new flow, so the old one, with its retries and its
    /// clear, is dropped on the next pass, and the new copy's clear runs.
    pub fn start(&mut self, text: SecretString) {
        self.next += 1;
        self.request = Some(CopyRequest {
            id: self.next,
            text: Some(text),
        });
    }

    /// Runs the newest copy as a flow: copies its text with the clipboard
    /// owned by the window whose `AppWindow.Id` is `window`, shows
    /// "Copied · clears in N s" while it waits, then clears the clipboard.
    /// Draws nothing while no copy runs, or while the clipboard is busy.
    /// Declare it on every pass where the status shows; a pass that leaves
    /// it out drops the copy's wait, so call [`clear`](Self::clear) then.
    pub fn status(&mut self, ui: &mut Ui, window: Option<u64>) -> windows_core::Result<()> {
        let Some(request) = &mut self.request else {
            return Ok(());
        };
        let text = &mut request.text;
        let mut flow = ui.flow(("copy", request.id), owned(&mut self.copied), |flow| {
            let text = text.take().expect("a copy's flow starts once");
            async move {
                let mut attempts = 0;
                let copied = loop {
                    match clipboard::copy(window, &text) {
                        Ok(copied) => break copied,
                        // Another app holds the clipboard for a moment, as
                        // monitors do after each change.
                        Err(CopyError::Busy) if attempts < 5 => {
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
                let deadline = Instant::now() + CLEAR_CLIPBOARD_AFTER;
                loop {
                    // Whole seconds left, rounded up, so the label reads 30
                    // first and 1 last.
                    let left = deadline.saturating_duration_since(Instant::now());
                    let seconds = left.as_millis().div_ceil(1000);
                    if seconds == 0 {
                        break;
                    }
                    // The first frame runs in the poll the copy landed in,
                    // because no frame came before it there, so a lock
                    // never sees a copy that `copied` does not record.
                    flow.frame(|ui, last| {
                        *last.get_mut() = Some(copied);
                        ui.label("copied", format_args!("Copied · clears in {seconds} s"))?
                            .secondary(true)?
                            .style("CaptionTextBlockStyle")?
                            .valign(Align::Center)?;
                        Ok(Some(()))
                    })
                    .await;
                    let next = u64::try_from(seconds - 1).expect("at most 30 seconds");
                    winui_executor::sleep_until(deadline - Duration::from_secs(next)).await;
                }
                clipboard::clear(copied);
                // A flow dropped before this frame leaves `copied` set, so
                // `clear` clears again, which the clipboard's sequence check
                // makes a no-op.
                flow.frame(|_, last| {
                    *last.get_mut() = None;
                    Ok(Some(()))
                })
                .await;
            }
        })?;
        // The last frame drew nothing, so the status is already empty, and
        // the next pass, whenever it comes, drops the finished flow.
        if flow.take().is_some() {
            self.request = None;
        }
        Ok(())
    }

    /// Empties the clipboard if it still holds the last copy, and drops a
    /// copy still trying or waiting. Call it when the vault locks.
    pub fn clear(&mut self) {
        self.request = None;
        if let Some(copied) = self.copied.take() {
            clipboard::clear(copied);
        }
    }
}
