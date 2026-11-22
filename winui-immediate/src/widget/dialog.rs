//! A `ContentDialog` the app declares only while it is open, as a menu.
//!
//! A dialog shows when the app first declares it, and the app keeps
//! declaring it until [`DialogResponse::answer`] gives an answer. A pass that
//! stops declaring it closes it.

use super::{boxed, resource};
use crate::{
    bindings::{ContentDialog, ContentDialogButton, FrameworkElement, Style},
    events::EventKind,
    pass::{ChildKind, DialogNode, NativeTree, Payload, Ui},
    tree::Visit,
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// A modal dialog over the window of the control around it: `title`,
    /// `message`, a `primary` button and a `close` button. The close button
    /// is the default: Enter picks it and it gets the accent, so a dialog
    /// that asks before a loss, such as Discard, defaults to the safe
    /// choice. Escape answers as `close` does.
    ///
    /// Declare it inside a control, whose window it covers. Only one dialog
    /// can be open in a window; a second one fails to show.
    pub fn dialog(
        &mut self,
        id: impl Hash,
        title: &str,
        message: &str,
        primary: &str,
        close: &str,
    ) -> Result<DialogResponse> {
        let owner = self.enclosing_control();
        let (node, visit) = self.visit(id, ChildKind::Logical, || {
            let dialog = ContentDialog::new()?;
            // WinUI 3 gives a dialog made in code no style of its own.
            dialog
                .cast::<FrameworkElement>()?
                .SetStyle(&resource::<Style>("DefaultContentDialogStyle")?)?;
            Ok(Payload::Dialog(Box::new(DialogNode {
                dialog,
                subscribed: Default::default(),
            })))
        })?;
        self.subscribe_native(
            node,
            &[EventKind::DialogPrimary, EventKind::DialogClosed],
            |payload| match payload {
                Payload::Dialog(dialog) => (&dialog.dialog, &mut dialog.subscribed),
                _ => unreachable!("dialog keys hold dialogs"),
            },
        )?;
        if visit == Visit::Created {
            let Payload::Dialog(dialog) = self.core.tree.native(node) else {
                unreachable!("dialog keys hold dialogs")
            };
            let control = &dialog.dialog;
            // A dialog shows once, with the text of its first pass, so its
            // text is written only here.
            control.SetTitle(&boxed(title))?;
            control
                .cast::<crate::bindings::ContentControl>()?
                .SetContent(&boxed(message))?;
            control.SetPrimaryButtonText(primary)?;
            control.SetCloseButtonText(close)?;
            control.SetDefaultButton(ContentDialogButton::Close)?;
            control
                .cast::<crate::bindings::UIElement>()?
                .SetXamlRoot(&owner.XamlRoot()?)?;
            // The answer comes back as events, so the operation is not
            // awaited; it ends when the dialog closes.
            control.ShowAsync()?;
        }
        let answer = match (
            self.events.happened(node, EventKind::DialogPrimary),
            self.events.happened(node, EventKind::DialogClosed),
        ) {
            (true, _) => Some(DialogAnswer::Primary),
            (false, true) => Some(DialogAnswer::Close),
            (false, false) => None,
        };
        Ok(DialogResponse { answer })
    }
}

/// What [`Ui::dialog`] returns.
pub struct DialogResponse {
    answer: Option<DialogAnswer>,
}

impl DialogResponse {
    /// The button the user picked since the last pass, if any. Stop
    /// declaring the dialog once it gives one.
    pub fn answer(&self) -> Option<DialogAnswer> {
        self.answer
    }
}

/// How the user closed a [`Ui::dialog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogAnswer {
    /// The primary button.
    Primary,
    /// The close button, or Escape.
    Close,
}
