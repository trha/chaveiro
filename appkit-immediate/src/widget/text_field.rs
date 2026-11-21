use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{msg_send, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSTextField, NSTextView};
use objc2_foundation::NSString;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A one-line text field bound to `value`.
    ///
    /// The field owns its text while the user edits: after the user types,
    /// the pass copies the field's text into `value`. The pass writes `value`
    /// into the field only when the app changed it since, and never while an
    /// input method composition is open, so the caret, the selection and the
    /// composition survive every pass. A write held back by a composition
    /// happens on the pass after it ends.
    pub fn text_field(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> TextFieldResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let field = NSTextField::new(mtm);
            let view = Retained::clone(&field);
            ControlNode::field(
                mtm,
                &view,
                Control::TextField {
                    field,
                    text: String::new(),
                    placeholder: String::new(),
                },
            )
        });
        if visit == Visit::Created {
            let Control::TextField { field, .. } = &self.control(node).control else {
                unreachable!("text field keys hold text fields")
            };
            self.events.register(field, node);
            // AppKit holds the delegate and target weakly; `Events` keeps the
            // shared target alive for as long as the field exists.
            unsafe {
                field.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
                field.setTarget(Some(self.events.target()));
                field.setAction(Some(objc2::sel!(fire:)));
            }
        }
        let typed = self.events.happened(node, EventKind::TextDidChange);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::TextField {
            field,
            text,
            placeholder: written_placeholder,
        } = &mut control.control
        else {
            unreachable!("text field keys hold text fields")
        };
        if must_write(node, stale, written_placeholder.as_str(), placeholder) {
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            written_placeholder.clear();
            written_placeholder.push_str(placeholder);
        }
        let mut changed = false;
        if typed {
            let current = field.stringValue().to_string();
            // An echo of this layer's own write reads back the stored text.
            if current != *text {
                value.clone_from(&current);
                *text = current;
                changed = true;
            }
        } else if must_write(node, stale, text.as_str(), value.as_str()) {
            // `setStringValue` during an edit keeps the caret and selection
            // (measured by `tools/smoke.sh`), but it would replace an open
            // composition's marked text, so that write waits.
            let composing = field.currentEditor().is_some_and(|editor| {
                let editor = editor
                    .downcast::<NSTextView>()
                    .expect("a field editor is a text view");
                unsafe { msg_send![&editor, hasMarkedText] }
            });
            if !composing {
                field.setStringValue(&NSString::from_str(value));
                text.clone_from(value);
            }
        }
        TextFieldResponse {
            response: Response { ui: self, node },
            changed,
        }
    }
}

/// What [`Ui::text_field`] returns.
pub struct TextFieldResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl TextFieldResponse<'_> {
    /// Whether the user changed the text since the last pass; `value` then
    /// holds the new text.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Whether the user pressed Return in the field since the last pass.
    pub fn return_pressed(&self) -> bool {
        let Response { ui, node } = &self.response;
        ui.events.happened(*node, EventKind::Action)
    }
}

impl<'u> Deref for TextFieldResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for TextFieldResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
