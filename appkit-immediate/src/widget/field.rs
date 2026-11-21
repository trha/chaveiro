//! The header and the error line around a form field.

use super::Response;
use crate::{pass::Ui, tree::NodeId};
use objc2::{MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::{
    NSColor, NSFont, NSImage, NSImageView, NSLayoutAttribute, NSLayoutConstraintOrientation,
    NSLineBreakMode, NSStackView, NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation,
    NSView,
};
use objc2_foundation::NSString;

/// A field's control with an optional header and an optional error line,
/// each made on first use. `stack` holds the header and `body`, and `body`
/// holds the control over the error line. Inside a [`Ui::form`] the header
/// sits beside `body`, else above it.
pub(crate) struct Field {
    pub(crate) stack: Retained<NSStackView>,
    body: Retained<NSStackView>,
    control: Retained<NSView>,
    header: Option<Retained<NSTextField>>,
    /// The header sits beside `body`, as a form row. Fixed when the header
    /// is made.
    beside: bool,
    /// The header as last declared, before the " *" a required field adds.
    header_text: String,
    required: bool,
    error: Option<ErrorLine>,
}

impl Field {
    /// Wraps `control` in a new field stack.
    pub(crate) fn new(mtm: MainThreadMarker, control: &NSView) -> Box<Self> {
        let stack = vertical(mtm);
        let body = vertical(mtm);
        body.setTranslatesAutoresizingMaskIntoConstraints(false);
        body.addArrangedSubview(control);
        stretch(&body, control);
        stack.addArrangedSubview(&body);
        stretch(&stack, &body);
        Box::new(Field {
            stack,
            body,
            control: control.retain(),
            header: None,
            beside: false,
            header_text: String::new(),
            required: false,
            error: None,
        })
    }

    /// Writes the header of `node`'s field when `text` or `required`
    /// changed.
    ///
    /// `side_width` is the header column's width inside a form. It takes effect
    /// when the header is made, so a field keeps its layout after that.
    fn set_header(
        &mut self,
        node: NodeId,
        mtm: MainThreadMarker,
        side_width: Option<f64>,
        text: &str,
        required: bool,
    ) {
        if self.header.is_some() && self.header_text == text && self.required == required {
            return;
        }
        crate::write_check::record(node);
        self.header_text.clear();
        self.header_text.push_str(text);
        self.required = required;
        if self.header.is_none() {
            self.beside = side_width.is_some();
        }
        // A Mac form labels its fields "Name:" and marks no required field.
        let shown = match (self.beside, required) {
            (true, _) => format!("{text}:"),
            (false, true) => format!("{text} *"),
            (false, false) => text.to_owned(),
        };
        let header = match &self.header {
            Some(header) => header,
            None => {
                let header = NSTextField::labelWithString(&NSString::new(), mtm);
                self.stack.insertArrangedSubview_atIndex(&header, 0);
                match side_width {
                    Some(width) => self.beside(&header, width),
                    None => stretch(&self.stack, &header),
                }
                self.header.insert(header)
            }
        };
        header.setStringValue(&NSString::from_str(&shown));
    }

    /// Lays the field out as a form row: `header`, `width` points wide and
    /// right-aligned, sits on the control's first baseline, beside `body`.
    fn beside(&self, header: &NSTextField, width: f64) {
        let stack = &self.stack;
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        // The constraints below place both rows across the stack.
        stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
        stack.setSpacing(8.0);
        header.setAlignment(NSTextAlignment::Right);
        for constraint in [
            header.widthAnchor().constraintEqualToConstant(width),
            header
                .firstBaselineAnchor()
                .constraintEqualToAnchor(&self.control.firstBaselineAnchor()),
            header
                .topAnchor()
                .constraintGreaterThanOrEqualToAnchor(&stack.topAnchor()),
            self.body
                .topAnchor()
                .constraintEqualToAnchor(&stack.topAnchor()),
            self.body
                .bottomAnchor()
                .constraintEqualToAnchor(&stack.bottomAnchor()),
        ] {
            constraint.setActive(true);
        }
    }

    fn error_line(&mut self, mtm: MainThreadMarker) -> &mut ErrorLine {
        self.error.get_or_insert_with(|| {
            let line = ErrorLine::new(mtm, false);
            self.body.addArrangedSubview(&line.row);
            stretch(&self.body, &line.row);
            line
        })
    }
}

/// A vertical stack whose rows start at its leading edge.
fn vertical(mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    // `Leading` pins each row's leading edge; `stretch` adds the trailing
    // one, since the `Width` alignment does not fill the stack.
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(4.0);
    stack
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
pub(crate) struct ErrorLine {
    pub(crate) row: Retained<NSStackView>,
    icon: Retained<NSImageView>,
    text: Retained<NSTextField>,
    /// The error shown; [`None`] while the field is fine.
    pub(crate) message: Option<String>,
    /// The caption shown while there is no error; empty for none.
    hint: String,
}

impl ErrorLine {
    /// `wrap` lets a long message wrap onto more lines, for a line that
    /// sits under the whole form.
    pub(crate) fn new(mtm: MainThreadMarker, wrap: bool) -> Self {
        let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("exclamationmark.circle"),
            Some(&NSString::from_str("Error")),
        )
        .expect("the SF Symbol exclamationmark.circle exists");
        let icon = NSImageView::imageViewWithImage(&symbol, mtm);
        icon.setContentTintColor(Some(&NSColor::systemRedColor()));
        let text = NSTextField::labelWithString(&NSString::new(), mtm);
        text.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        if wrap {
            text.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
            text.setUsesSingleLineMode(false);
            text.setMaximumNumberOfLines(0);
            // Without this the label asks for its one-line width and widens
            // its container.
            text.setContentCompressionResistancePriority_forOrientation(
                1.0,
                NSLayoutConstraintOrientation::Horizontal,
            );
        }
        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(4.0);
        row.addArrangedSubview(&icon);
        row.addArrangedSubview(&text);
        row.heightAnchor()
            .constraintGreaterThanOrEqualToConstant(16.0)
            .setActive(true);
        if wrap {
            // The line takes the room its row leaves, such as the space left
            // of a form's buttons.
            row.setHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
        }
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

    pub(crate) fn render(&self) {
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

    /// Writes the text shown above a field, or beside it inside a
    /// [`Ui::form`], where it reads "`text`:".
    ///
    /// Panics when the control is not a field.
    pub fn header(&mut self, text: &str) -> &mut Self {
        let (mtm, node, side) = (self.ui.mtm, self.node, self.ui.form_header_width);
        let field = self.field_mut();
        let required = field.required;
        field.set_header(node, mtm, side, text, required);
        self
    }

    /// Marks a field as required, which adds " *" to its header outside a
    /// [`Ui::form`].
    ///
    /// Panics when the control is not a field.
    pub fn required(&mut self, on: bool) -> &mut Self {
        let (mtm, node, side) = (self.ui.mtm, self.node, self.ui.form_header_width);
        let field = self.field_mut();
        if field.required == on {
            return self;
        }
        // `set_header` reads the text from an argument, so the field's own
        // copy moves out; `set_header` writes it back.
        let text = std::mem::take(&mut field.header_text);
        field.set_header(node, mtm, side, &text, on);
        self
    }

    /// Writes the error line under a field, or clears it with [`None`]. The
    /// first call reserves the line, so a later error never moves the
    /// controls below; call it with [`None`] on a field that is fine to keep
    /// its place from the start.
    ///
    /// Panics when the control is not a field.
    pub fn error(&mut self, message: Option<&str>) -> &mut Self {
        let (mtm, node) = (self.ui.mtm, self.node);
        let line = self.field_mut().error_line(mtm);
        if line.message.as_deref() != message {
            crate::write_check::record(node);
            line.message = message.map(str::to_owned);
            line.render();
        }
        self
    }

    /// Writes the caption shown on the error line while there is no error.
    ///
    /// Panics when the control is not a field.
    pub fn hint(&mut self, text: &str) -> &mut Self {
        let (mtm, node) = (self.ui.mtm, self.node);
        let line = self.field_mut().error_line(mtm);
        if line.hint != text {
            crate::write_check::record(node);
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
