//! The header and the error line around a form field.

use super::Response;
use crate::{pass::Ui, tree::NodeId};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSColor, NSFont, NSImage, NSImageView, NSLayoutAttribute, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::NSString;

/// A field's control in a vertical stack: an optional header above it and
/// an optional error line below it, each made on first use.
pub(crate) struct Field {
    pub(crate) stack: Retained<NSStackView>,
    header: Option<Retained<NSTextField>>,
    /// The header as last declared, before the " *" a required field adds.
    header_text: String,
    required: bool,
    error: Option<ErrorLine>,
}

impl Field {
    /// Wraps `control` in a new field stack.
    pub(crate) fn new(mtm: MainThreadMarker, control: &NSView) -> Box<Self> {
        let stack = NSStackView::new(mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        // `Leading` pins each row's leading edge; `stretch` adds the
        // trailing one, since the `Width` alignment does not fill the stack.
        stack.setAlignment(NSLayoutAttribute::Leading);
        stack.setSpacing(4.0);
        stack.addArrangedSubview(control);
        stretch(&stack, control);
        Box::new(Field {
            stack,
            header: None,
            header_text: String::new(),
            required: false,
            error: None,
        })
    }

    fn set_header(&mut self, mtm: MainThreadMarker, text: &str, required: bool) {
        if self.header.is_some() && self.header_text == text && self.required == required {
            return;
        }
        self.header_text.clear();
        self.header_text.push_str(text);
        self.required = required;
        let shown = if required {
            format!("{text} *")
        } else {
            text.to_owned()
        };
        let header = self.header.get_or_insert_with(|| {
            let header = NSTextField::labelWithString(&NSString::new(), mtm);
            self.stack.insertArrangedSubview_atIndex(&header, 0);
            stretch(&self.stack, &header);
            header
        });
        header.setStringValue(&NSString::from_str(&shown));
    }

    fn error_line(&mut self, mtm: MainThreadMarker) -> &mut ErrorLine {
        self.error.get_or_insert_with(|| {
            let line = ErrorLine::new(mtm);
            self.stack.addArrangedSubview(&line.row);
            stretch(&self.stack, &line.row);
            line
        })
    }
}

/// Pins `row`'s trailing edge to `stack`'s, so the row fills the field.
fn stretch(stack: &NSStackView, row: &NSView) {
    row.trailingAnchor()
        .constraintEqualToAnchor(&stack.trailingAnchor())
        .setActive(true);
}

/// The line under a field: an error with its icon in red, or else the hint
/// in the secondary color. Its height is reserved from its first use, so a
/// later error never moves the controls below.
struct ErrorLine {
    row: Retained<NSStackView>,
    icon: Retained<NSImageView>,
    text: Retained<NSTextField>,
    /// The error shown; [`None`] while the field is fine.
    message: Option<String>,
    /// The caption shown while there is no error; empty for none.
    hint: String,
}

impl ErrorLine {
    fn new(mtm: MainThreadMarker) -> Self {
        let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("exclamationmark.circle"),
            Some(&NSString::from_str("Error")),
        )
        .expect("the SF Symbol exclamationmark.circle exists");
        let icon = NSImageView::imageViewWithImage(&symbol, mtm);
        icon.setContentTintColor(Some(&NSColor::systemRedColor()));
        let text = NSTextField::labelWithString(&NSString::new(), mtm);
        text.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(4.0);
        row.addArrangedSubview(&icon);
        row.addArrangedSubview(&text);
        row.heightAnchor()
            .constraintGreaterThanOrEqualToConstant(16.0)
            .setActive(true);
        let line = ErrorLine {
            row,
            icon,
            text,
            message: None,
            hint: String::new(),
        };
        line.render();
        line
    }

    fn render(&self) {
        let (text, color) = match &self.message {
            Some(message) => (message.as_str(), NSColor::systemRedColor()),
            None => (self.hint.as_str(), NSColor::secondaryLabelColor()),
        };
        self.icon.setHidden(self.message.is_none());
        self.text.setStringValue(&NSString::from_str(text));
        self.text.setTextColor(Some(&color));
    }
}

/// Names a text field, secure text field, pop-up button or combo box during
/// the pass that declared it, for changes decided after later controls,
/// such as an error found when the form's button is read.
#[derive(Clone, Copy, Debug)]
pub struct FieldHandle(pub(crate) NodeId);

impl Ui {
    /// [`Response::error`] for the field `field` names.
    pub fn set_error(&mut self, field: FieldHandle, message: Option<&str>) {
        Response {
            ui: self,
            node: field.0,
        }
        .error(message);
    }

    /// [`Response::make_first_responder`] for the field `field` names.
    pub fn make_first_responder(&mut self, field: FieldHandle) {
        Response {
            ui: self,
            node: field.0,
        }
        .make_first_responder();
    }
}

impl Response<'_> {
    /// A handle to this field, for [`Ui::set_error`] and
    /// [`Ui::make_first_responder`].
    pub fn field(&self) -> FieldHandle {
        FieldHandle(self.node)
    }

    /// Writes the text shown above a field.
    ///
    /// Panics when the control is not a field.
    pub fn header(&mut self, text: &str) -> &mut Self {
        let mtm = self.ui.mtm;
        let field = self.field_mut();
        let required = field.required;
        field.set_header(mtm, text, required);
        self
    }

    /// Marks a field as required, which adds " *" to its header.
    ///
    /// Panics when the control is not a field.
    pub fn required(&mut self, on: bool) -> &mut Self {
        let mtm = self.ui.mtm;
        let field = self.field_mut();
        if field.required == on {
            return self;
        }
        // `set_header` reads the text from an argument, so the field's own
        // copy moves out; `set_header` writes it back.
        let text = std::mem::take(&mut field.header_text);
        field.set_header(mtm, &text, on);
        self
    }

    /// Writes the error line under a field, or clears it with [`None`]. The
    /// first call reserves the line, so a later error never moves the
    /// controls below; call it with [`None`] on a field that is fine to keep
    /// its place from the start.
    ///
    /// Panics when the control is not a field.
    pub fn error(&mut self, message: Option<&str>) -> &mut Self {
        let mtm = self.ui.mtm;
        let line = self.field_mut().error_line(mtm);
        if line.message.as_deref() != message {
            line.message = message.map(str::to_owned);
            line.render();
        }
        self
    }

    /// Writes the caption shown on the error line while there is no error.
    ///
    /// Panics when the control is not a field.
    pub fn hint(&mut self, text: &str) -> &mut Self {
        let mtm = self.ui.mtm;
        let line = self.field_mut().error_line(mtm);
        if line.hint != text {
            line.hint.clear();
            line.hint.push_str(text);
            line.render();
        }
        self
    }

    /// Moves the keyboard focus to the control. Does nothing while the
    /// control is not in a window.
    pub fn make_first_responder(&mut self) -> &mut Self {
        let control = self.ui.control(self.node);
        let view = control
            .control
            .as_control()
            .map_or_else(|| control.view.clone(), Retained::into_super);
        if let Some(window) = view.window() {
            window.makeFirstResponder(Some(&view));
        }
        self
    }

    fn field_mut(&mut self) -> &mut Field {
        self.ui.control_mut(self.node).field.as_deref_mut().expect(
            "only text fields, secure text fields, pop-up buttons and combo boxes are fields",
        )
    }
}
