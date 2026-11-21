use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{rc::Retained, sel};
use objc2_app_kit::{NSButton, NSControlStateValueOff, NSControlStateValueOn};
use objc2_foundation::NSString;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A check box showing `label`, bound to `on`.
    ///
    /// A click copies the new state into `on`. The pass writes `on` into the
    /// box only when the app changed it. The box is a field, so inside a
    /// [`Ui::form`] an empty [`header`](Response::header) puts it in the
    /// fields' column.
    pub fn check_box(&mut self, id: impl Hash, label: &str, on: &mut bool) -> CheckBoxResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let button = unsafe {
                NSButton::checkboxWithTitle_target_action(&NSString::new(), None, None, mtm)
            };
            let view = Retained::clone(&button);
            ControlNode::field(
                mtm,
                &view,
                Control::CheckBox {
                    button,
                    label: String::new(),
                    on: false,
                },
            )
        });
        if visit == Visit::Created {
            let Control::CheckBox { button, .. } = &self.control(node).control else {
                unreachable!("check box keys hold check boxes")
            };
            self.events.register(button, node);
            // AppKit holds the target weakly; `Events` keeps it alive.
            unsafe {
                button.setTarget(Some(self.events.target()));
                button.setAction(Some(sel!(fire:)));
            }
        }

        let clicked = self.events.happened(node, EventKind::Action);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::CheckBox {
            button,
            label: written_label,
            on: written,
        } = &mut control.control
        else {
            unreachable!("check box keys hold check boxes")
        };

        if must_write(node, stale, written_label.as_str(), label) {
            button.setTitle(&NSString::from_str(label));
            written_label.clear();
            written_label.push_str(label);
        }

        let mut changed = false;
        if clicked {
            let state = button.state() == NSControlStateValueOn;
            if state != *written {
                *written = state;
                *on = state;
                changed = true;
            }
        }

        if must_write(node, stale, written, on) {
            button.setState(match *on {
                true => NSControlStateValueOn,
                false => NSControlStateValueOff,
            });
            *written = *on;
        }

        CheckBoxResponse {
            response: Response { ui: self, node },
            changed,
        }
    }
}

/// What [`Ui::check_box`] returns.
pub struct CheckBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl CheckBoxResponse<'_> {
    /// Whether the user changed the state since the last pass; `on` then
    /// holds the new state.
    pub fn changed(&self) -> bool {
        self.changed
    }
}

impl<'u> Deref for CheckBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for CheckBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
