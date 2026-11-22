use super::{Control, Response, must_write};
use crate::{
    bindings::{FrameworkElement, ITextBox, TextBox, TextWrapping},
    events::{EventKind, Key},
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
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
        let capacity = value.capacity();
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            Ok(ControlNode::new(
                text_box.cast()?,
                Control::TextBox {
                    text_box,
                    // As roomy as the app's buffer, so the layer's copy
                    // does not grow on a keystroke either.
                    text: String::with_capacity(capacity),
                    placeholder: String::new(),
                    composing: false,
                },
            ))
        })?;
        let changed = self.bind_text(node, placeholder, value)?;
        Ok(TextBoxResponse {
            response: Response { ui: self, node },
            changed,
        })
    }

    /// A text box for several lines, such as a note: Enter starts a new
    /// line, so [`TextBoxResponse::enter_pressed`] never fires, and long
    /// lines wrap. Otherwise it binds `value` as [`text_box`](Self::text_box)
    /// does.
    pub fn text_area(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<TextBoxResponse<'_>> {
        let capacity = value.capacity();
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            text_box.SetAcceptsReturn(true)?;
            text_box.SetTextWrapping(TextWrapping::Wrap)?;
            text_box.cast::<FrameworkElement>()?.SetMinHeight(80.0)?;
            Ok(ControlNode::new(
                text_box.cast()?,
                Control::TextBox {
                    text_box,
                    // As roomy as the app's buffer, so the layer's copy
                    // does not grow on a keystroke either.
                    text: String::with_capacity(capacity),
                    placeholder: String::new(),
                    composing: false,
                },
            ))
        })?;
        let changed = self.bind_text(node, placeholder, value)?;
        Ok(TextBoxResponse {
            response: Response { ui: self, node },
            changed,
        })
    }

    /// Syncs the text box of `node`, a [`Control::TextBox`] or
    /// [`Control::SearchBox`], with `placeholder` and `value`, as
    /// [`Ui::text_box`] describes. Returns whether the user changed the
    /// text.
    pub(crate) fn bind_text(
        &mut self,
        node: NodeId,
        placeholder: &str,
        value: &mut String,
    ) -> Result<bool> {
        let control = self.control_mut(node);
        let (Control::TextBox { text_box, .. } | Control::SearchBox { text_box, .. }) =
            &control.control
        else {
            unreachable!("text keys hold text boxes")
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
        let (Control::TextBox {
            text,
            placeholder: written_placeholder,
            composing,
            ..
        }
        | Control::SearchBox {
            text,
            placeholder: written_placeholder,
            composing,
            ..
        }) = &mut control.control
        else {
            unreachable!("text keys hold text boxes")
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
            let current = text_box.cast::<ITextBox>()?.text_string()?;
            // An echo of this layer's own write reads back the stored text.
            // The compare and the copies reuse the strings' buffers, so a
            // keystroke allocates only when the text outgrows them.
            let units = || char::decode_utf16(current.iter().copied());
            let same = units()
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .eq(text.chars());
            if !same {
                for target in [&mut *value, &mut *text] {
                    target.clear();
                    target.extend(units().map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)));
                }
                changed = true;
            }
        } else if !*composing && must_write(stale, text.as_str(), value.as_str()) {
            text_box.SetText(value)?;
            text.clone_from(value);
        }
        Ok(changed)
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

    /// Writes the text shown above the box.
    pub fn header(&mut self, text: &str) -> Result<&mut Self> {
        self.response.set_header(text)?;
        Ok(self)
    }

    /// Marks the field as required: ` *` after the header, and screen
    /// readers say so.
    pub fn required(&mut self, on: bool) -> Result<&mut Self> {
        self.response.set_required(on)?;
        Ok(self)
    }
    /// Shows `message` on the line under the box, or keeps the line empty;
    /// see [`Response`]'s error line. The first call reserves the line.
    pub fn error(&mut self, message: Option<&str>) -> Result<&mut Self> {
        self.response.set_error(message)?;
        Ok(self)
    }

    /// Whether the user pressed Enter in the box since the last pass. The
    /// first call subscribes the box.
    pub fn enter_pressed(&mut self) -> Result<bool> {
        enter_pressed(&mut self.response)
    }

    /// The [`Key`]s the user pressed in the box since the last pass, in
    /// order, seen before the box acted on them: a Backspace that deleted
    /// nothing is here too. The first call subscribes the box, so its first
    /// result is empty.
    pub fn keys(&mut self) -> Result<impl Iterator<Item = Key> + '_> {
        let Response { ui, node } = &mut self.response;
        let control = ui.control_mut(*node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        ui.events
            .subscribe(*node, &mut subscribed, &element, EventKind::Key)?;
        ui.control_mut(*node).subscribed = subscribed;
        Ok(ui.events.keys(*node))
    }
}

/// Subscribes the control of `response` to Enter and reads it.
pub(crate) fn enter_pressed(response: &mut Response<'_>) -> Result<bool> {
    let Response { ui, node } = response;
    let control = ui.control_mut(*node);
    let element = control.element.clone();
    let mut subscribed = control.subscribed;
    ui.events
        .subscribe(*node, &mut subscribed, &element, EventKind::Enter)?;
    ui.control_mut(*node).subscribed = subscribed;
    Ok(ui.events.happened(*node, EventKind::Enter))
}

impl<'u> Deref for TextBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for TextBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
