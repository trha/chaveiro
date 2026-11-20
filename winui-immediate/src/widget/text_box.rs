use super::{Control, Response, must_write};
use crate::{
    bindings::TextBox,
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{hash::Hash, ops::Deref};
use windows_core::{Interface, Result};

impl Ui {
    /// A one-line text box bound to `value`.
    ///
    /// The control owns its text while the user edits: after the user types,
    /// the pass copies the control's text into `value`. The pass writes
    /// `value` into the control only when the app changed it since, and never
    /// while an IME composition is open, so the caret, the selection and the
    /// composition survive every pass. A write held back by a composition
    /// happens on the pass after it ends.
    pub fn text_box(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<TextBoxResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            Ok(ControlNode::new(
                text_box.cast()?,
                Control::TextBox {
                    text_box,
                    text: String::new(),
                    placeholder: String::new(),
                    composing: false,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let Control::TextBox { text_box, .. } = &control.control else {
            unreachable!("text box keys hold text boxes")
        };
        let text_box = text_box.clone();
        let mut subscribed = control.subscribed;
        for kind in [
            EventKind::TextChanged,
            EventKind::CompositionStarted,
            EventKind::CompositionEnded,
        ] {
            self.events
                .subscribe(node, &mut subscribed, &text_box, kind)?;
        }
        let typed = self.events.happened(node, EventKind::TextChanged);
        let composition = self.events.last_of(
            node,
            EventKind::CompositionStarted,
            EventKind::CompositionEnded,
        );
        let control = self.control_mut(node);
        control.subscribed = subscribed;
        let stale = std::mem::take(&mut control.stale);
        let Control::TextBox {
            text,
            placeholder: written_placeholder,
            composing,
            ..
        } = &mut control.control
        else {
            unreachable!("text box keys hold text boxes")
        };
        if let Some(kind) = composition {
            *composing = kind == EventKind::CompositionStarted;
        }
        if must_write(stale, written_placeholder.as_str(), placeholder) {
            text_box.SetPlaceholderText(placeholder)?;
            written_placeholder.clear();
            written_placeholder.push_str(placeholder);
        }
        let mut changed = false;
        if typed {
            let current = text_box.Text()?;
            // An echo of this layer's own write reads back the stored text.
            if current != *text {
                value.clone_from(&current);
                *text = current;
                changed = true;
            }
        } else if !*composing && must_write(stale, text.as_str(), value.as_str()) {
            text_box.SetText(value)?;
            text.clone_from(value);
        }
        Ok(TextBoxResponse {
            response: Response { ui: self, node },
            changed,
        })
    }
}

/// What [`Ui::text_box`] returns.
pub struct TextBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl TextBoxResponse<'_> {
    /// Whether the user changed the text since the last pass; `value` then
    /// holds the new text.
    pub fn changed(&self) -> bool {
        self.changed
    }
}

impl<'u> Deref for TextBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}
