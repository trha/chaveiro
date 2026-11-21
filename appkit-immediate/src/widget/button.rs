use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{rc::Retained, sel};
use objc2_app_kit::NSButton;
use objc2_foundation::NSString;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A push button showing `label`.
    pub fn button(&mut self, id: impl Hash, label: &str) -> ButtonResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::new(), None, None, mtm)
            };
            ControlNode::payload(
                Retained::clone(&button).into_super().into_super(),
                Control::Button {
                    button,
                    label: String::new(),
                },
            )
        });
        if visit == Visit::Created {
            // Registering needs the node id, which exists only now.
            let control = self.control(node);
            let Control::Button { button, .. } = &control.control else {
                unreachable!("button keys hold buttons")
            };
            self.events.register(button, node);
            unsafe {
                button.setTarget(Some(self.events.target()));
                button.setAction(Some(sel!(fire:)));
            }
        }
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Button {
            button,
            label: written,
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, written.as_str(), label) {
            button.setTitle(&NSString::from_str(label));
            written.clear();
            written.push_str(label);
        }
        ButtonResponse(Response { ui: self, node })
    }
}

/// What [`Ui::button`] returns.
pub struct ButtonResponse<'u>(Response<'u>);

impl ButtonResponse<'_> {
    /// Whether the button was clicked since the last pass.
    pub fn clicked(&self) -> bool {
        self.0.ui.events.happened(self.0.node, EventKind::Click)
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
