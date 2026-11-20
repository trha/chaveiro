use super::{Control, Response, must_write};
use crate::{
    bindings::{Button, ContentControl, TextBlock},
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A push button showing `label`.
    pub fn button(&mut self, id: impl Hash, label: &str) -> Result<ButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = Button::new()?;
            let content = TextBlock::new()?;
            button.cast::<ContentControl>()?.SetContent(&content)?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::Button {
                    button,
                    content,
                    label: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Button {
            content,
            label: written,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, written.as_str(), label) {
            content.SetText(label)?;
            written.clear();
            written.push_str(label);
        }
        Ok(ButtonResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::button`] returns.
pub struct ButtonResponse<'u>(Response<'u>);

impl ButtonResponse<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// subscribes the button, so a button whose clicks are never asked for
    /// registers no handler.
    pub fn clicked(&mut self) -> Result<bool> {
        let Response { ui, node } = &mut self.0;
        let control = ui.control_mut(*node);
        let Control::Button { button, .. } = &control.control else {
            unreachable!("button keys hold buttons")
        };
        let button = button.clone();
        let mut subscribed = control.subscribed;
        ui.events
            .subscribe(*node, &mut subscribed, &button, EventKind::Click)?;
        ui.control_mut(*node).subscribed = subscribed;
        Ok(ui.events.happened(*node, EventKind::Click))
    }
}

impl<'u> Deref for ButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for ButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
