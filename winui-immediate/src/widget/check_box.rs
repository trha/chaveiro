use super::{Control, Response, must_write};
use crate::{
    bindings::{CheckBox, ContentControl, TextBlock},
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A check box showing `label`, bound to `checked`. A click by the user
    /// reaches `checked` on the pass after it; the pass writes `checked`
    /// into the box only when the app changed it.
    pub fn check_box(
        &mut self,
        id: impl Hash,
        label: &str,
        checked: &mut bool,
    ) -> Result<CheckBoxResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let check_box = CheckBox::new()?;
            let content = TextBlock::new()?;
            check_box.cast::<ContentControl>()?.SetContent(&content)?;
            Ok(ControlNode::new(
                check_box.cast()?,
                Control::CheckBox {
                    check_box: check_box.cast()?,
                    content,
                    label: String::new(),
                    checked: false,
                },
            ))
        })?;
        self.subscribe(node, &[EventKind::Click], |control| &control.element)?;
        let clicked = self.events.happened(node, EventKind::Click);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::CheckBox {
            check_box,
            content,
            label: written_label,
            checked: written_checked,
        } = &mut control.control
        else {
            unreachable!("check box keys hold check boxes")
        };
        if must_write(stale, written_label.as_str(), label) {
            content.SetText(label)?;
            written_label.clear();
            written_label.push_str(label);
        }
        let mut changed = false;
        if clicked {
            let current = check_box.IsChecked()?;
            if current != *written_checked {
                *written_checked = current;
                *checked = current;
                changed = true;
            }
        } else if must_write(stale, written_checked, checked) {
            check_box.SetIsChecked(Some(*checked))?;
            *written_checked = *checked;
        }
        Ok(CheckBoxResponse {
            response: Response { ui: self, node },
            changed,
        })
    }
}

/// What [`Ui::check_box`] returns.
pub struct CheckBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl CheckBoxResponse<'_> {
    /// Whether the user changed the box since the last pass; `checked`
    /// then holds the new state.
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
