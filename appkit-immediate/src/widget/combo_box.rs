use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{rc::Retained, runtime::ProtocolObject, sel};
use objc2_app_kit::NSComboBox;
use objc2_foundation::{NSArray, NSString};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// An editable combo box that suggests `items`, bound to `text`.
    ///
    /// Picking an item puts it in the box. The box's text reaches `text` on
    /// Return, on a pick, or when the keyboard focus leaves the box. The
    /// pass writes `text` into the box only when the app changed it.
    pub fn combo_box(
        &mut self,
        id: impl Hash,
        items: &[&str],
        text: &mut String,
    ) -> ComboBoxResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let combo_box = NSComboBox::new(mtm);
            combo_box.setCompletes(true);
            let view = Retained::clone(&combo_box);
            ControlNode::field(
                mtm,
                &view,
                Control::ComboBox {
                    combo_box,
                    items: Vec::new(),
                    text: String::new(),
                },
            )
        });
        if visit == Visit::Created {
            let Control::ComboBox { combo_box, .. } = &self.control(node).control else {
                unreachable!("combo box keys hold combo boxes")
            };
            self.events.register(combo_box, node);
            // AppKit holds the delegate and target weakly; `Events` keeps
            // the shared target alive.
            unsafe {
                combo_box.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
                combo_box.setTarget(Some(self.events.target()));
                combo_box.setAction(Some(sel!(fire:)));
            }
        }
        let submitted = self.events.happened(node, EventKind::Action)
            || self.events.happened(node, EventKind::DidEndEditing);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::ComboBox {
            combo_box,
            items: written_items,
            text: written,
        } = &mut control.control
        else {
            unreachable!("combo box keys hold combo boxes")
        };
        if stale
            || written_items
                .iter()
                .map(String::as_str)
                .ne(items.iter().copied())
        {
            combo_box.removeAllItems();
            let values: Vec<Retained<NSString>> =
                items.iter().map(|item| NSString::from_str(item)).collect();
            let values = NSArray::from_retained_slice(&values);
            // An NSArray of strings is an NSArray of objects, and the box
            // copies what it needs.
            unsafe { combo_box.addItemsWithObjectValues(values.cast_unchecked()) };
            written_items.clear();
            written_items.extend(items.iter().map(|item| (*item).to_owned()));
        }
        let mut changed = false;
        if submitted {
            let current = combo_box.stringValue().to_string();
            if current != *written {
                text.clone_from(&current);
                *written = current;
                changed = true;
            }
        } else if must_write(stale, written.as_str(), text.as_str()) {
            combo_box.setStringValue(&NSString::from_str(text));
            written.clone_from(text);
        }
        ComboBoxResponse {
            response: Response { ui: self, node },
            changed,
        }
    }
}

/// What [`Ui::combo_box`] returns.
pub struct ComboBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl ComboBoxResponse<'_> {
    /// Whether the box submitted new text since the last pass; `text` then
    /// holds it.
    pub fn changed(&self) -> bool {
        self.changed
    }
}

impl<'u> Deref for ComboBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for ComboBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
