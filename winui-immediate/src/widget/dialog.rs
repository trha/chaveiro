//! A `ContentDialog` the app declares only while it is open, as a menu.
//!
//! A dialog shows when the app first declares it, and the app keeps
//! declaring it until [`DialogResponse::answer`] gives an answer. A pass that
//! stops declaring it closes it.

use super::{Overflow, boxed, resource};
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
        let buttons = DialogButtons {
            primary,
            secondary: None,
            close,
            default: DialogAnswer::Close,
        };
        self.content_dialog(id, title, buttons, |ui| {
            ui.label("message", format_args!("{message}"))?
                .overflow(Overflow::Wrap)?;
            Ok(())
        })
    }

    /// A modal dialog as [`dialog`](Self::dialog) opens, whose body is the
    /// one control `body` declares. The app declares the body on every pass
    /// while the dialog is open, so it can change, as when the user shows a
    /// hidden value. The title and buttons are those of the first pass.
    pub fn content_dialog(
        &mut self,
        id: impl Hash,
        title: &str,
        buttons: DialogButtons<'_>,
        body: impl FnOnce(&mut Ui) -> Result<()>,
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
                content: None,
                subscribed: Default::default(),
            })))
        })?;
        self.subscribe_native(
            node,
            &[
                EventKind::DialogPrimary,
                EventKind::DialogSecondary,
                EventKind::DialogClosed,
            ],
            |payload| match payload {
                Payload::Dialog(dialog) => (&dialog.dialog, &mut dialog.subscribed),
                _ => unreachable!("dialog keys hold dialogs"),
            },
        )?;
        self.open(node);
        body(self)?;
        self.close()?;
        if visit == Visit::Created {
            let Payload::Dialog(dialog) = self.core.tree.native(node) else {
                unreachable!("dialog keys hold dialogs")
            };
            let control = &dialog.dialog;
            // A dialog shows once, so its title and buttons are written
            // only here.
            control.SetTitle(&boxed(title))?;
            control.SetPrimaryButtonText(buttons.primary)?;
            if let Some(secondary) = buttons.secondary {
                control.SetSecondaryButtonText(secondary)?;
            }
            control.SetCloseButtonText(buttons.close)?;
            control.SetDefaultButton(match buttons.default {
                DialogAnswer::Primary => ContentDialogButton::Primary,
                DialogAnswer::Secondary => ContentDialogButton::Secondary,
                DialogAnswer::Close => ContentDialogButton::Close,
            })?;
            control
                .cast::<crate::bindings::UIElement>()?
                .SetXamlRoot(&owner.XamlRoot()?)?;
            // The answer comes back as events, so the operation is not
            // awaited; it ends when the dialog closes.
            control.ShowAsync()?;
        }
        // A button click comes with the close that follows it.
        let happened = |kind| self.events.happened(node, kind);
        let answer = if happened(EventKind::DialogPrimary) {
            Some(DialogAnswer::Primary)
        } else if happened(EventKind::DialogSecondary) {
            Some(DialogAnswer::Secondary)
        } else if happened(EventKind::DialogClosed) {
            Some(DialogAnswer::Close)
        } else {
            None
        };
        Ok(DialogResponse { answer })
    }
}

/// The buttons of a [`Ui::content_dialog`].
#[derive(Clone, Copy)]
pub struct DialogButtons<'a> {
    pub primary: &'a str,
    /// A third button between the primary and the close one.
    pub secondary: Option<&'a str>,
    pub close: &'a str,
    /// The button Enter picks, which gets the accent. A secondary default
    /// with no secondary button leaves the dialog without one.
    pub default: DialogAnswer,
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
    /// The secondary button.
    Secondary,
    /// The close button, or Escape.
    Close,
}
