//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the view the first time, and writes
//! a managed property only when its value differs from the value this layer
//! last wrote.

use crate::{events::EventKind, pass::Ui, tree::NodeId};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
    NSButton, NSColor, NSComboBox, NSControl, NSFont, NSFontDescriptorSymbolicTraits,
    NSFontTextStyleBody, NSFontTextStyleCaption1, NSFontTextStyleTitle1, NSFontTextStyleTitle2,
    NSLayoutConstraint, NSPopUpButton, NSProgressIndicator, NSSecureTextField, NSStackView,
    NSTableView, NSTextField, NSView,
};
use objc2_foundation::{NSDictionary, NSEdgeInsets};

mod alert_line;
mod button;
mod cache;
mod check_box;
mod code_boxes;
mod combo_box;
mod field;
mod grid;
mod label;
mod native;
mod pop_up_button;
mod secure_text_field;
mod stack_view;
mod table_view;
mod text_field;
mod window;

pub use button::ButtonResponse;
pub use check_box::CheckBoxResponse;
pub use code_boxes::{CODE_LENGTH, CodeBoxesResponse, CodeDigits};
pub use combo_box::ComboBoxResponse;
pub use field::FieldHandle;
pub(crate) use field::{ErrorLine, Field};
pub use grid::Length;
pub(crate) use grid::{Rule as GridRule, constraints as grid_constraints, rules as grid_rules};
pub use pop_up_button::PopUpButtonResponse;
pub use secure_text_field::{SecureTextFieldHandle, SecureTextFieldResponse};
pub use stack_view::Orientation;
pub(crate) use table_view::TableData;
pub use table_view::{RowFn, TableViewId};
pub use text_field::TextFieldResponse;
pub use window::WindowResponse;

/// Access to the view a widget declared, for what the widget does not cover.
///
/// A property this crate manages (a label's text, a stack's spacing) is
/// written again only when its declared value changes. After writing one
/// through [`view`](Self::view), call [`invalidate`](Self::invalidate), or
/// the next change back to the old value is skipped.
pub struct Response<'u> {
    pub(crate) ui: &'u mut Ui,
    pub(crate) node: NodeId,
}

impl Response<'_> {
    /// The view. Downcast it with `objc2` to reach its concrete class.
    pub fn view(&self) -> Retained<NSView> {
        self.ui.control(self.node).view.clone()
    }

    /// Makes the next pass write every managed property of this view, such
    /// as after writing one of them through [`view`](Self::view).
    pub fn invalidate(&mut self) {
        self.ui.invalidated = true;
        let control = self.ui.control_mut(self.node);
        control.stale = true;
        control.enabled = None;
        control.style = None;
        control.padding = None;
        control.text_color = None;
    }

    /// Sets whether the user can interact with the control. Code boxes
    /// take it for every box.
    ///
    /// Panics on any other view that is not an `NSControl`, such as a stack
    /// view.
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if must_write(self.node, false, &control.enabled, &Some(on)) {
            match &control.control {
                Control::CodeBoxes { boxes, .. } => {
                    for field in boxes {
                        field.setEnabled(on);
                    }
                }
                other => other
                    .as_control()
                    .expect("only controls can be disabled")
                    .setEnabled(on),
            }
            control.enabled = Some(on);
        }
        self
    }

    /// Sets the font of a label, text field or button to a text style.
    ///
    /// Panics when the view is not an `NSControl`.
    pub fn style(&mut self, style: Style) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if must_write(self.node, false, &control.style, &Some(style)) {
            let text_style = match style {
                Style::Title => unsafe { NSFontTextStyleTitle1 },
                Style::Title2 | Style::Heading => unsafe { NSFontTextStyleTitle2 },
                Style::Body => unsafe { NSFontTextStyleBody },
                Style::Caption => unsafe { NSFontTextStyleCaption1 },
            };
            // An empty options dictionary asks for the default font.
            let font = unsafe {
                NSFont::preferredFontForTextStyle_options(text_style, &NSDictionary::new())
            };
            let font = match style {
                Style::Heading => {
                    let descriptor = font.fontDescriptor().fontDescriptorWithSymbolicTraits(
                        NSFontDescriptorSymbolicTraits::TraitBold,
                    );
                    NSFont::fontWithDescriptor_size(&descriptor, 0.0)
                        .expect("the system font has a bold face")
                }
                _ => font,
            };
            control
                .control
                .as_control()
                .expect("only controls have a font")
                .setFont(Some(&font));
            control.style = Some(style);
        }
        self
    }

    /// Sets the text color of a label.
    ///
    /// Panics when the view is not a label.
    pub fn text_color(&mut self, color: TextColor) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if must_write(self.node, false, &control.text_color, &Some(color)) {
            let Control::Label { field, .. } = &control.control else {
                panic!("only labels take a text color")
            };
            let ns_color = match color {
                TextColor::Label => NSColor::labelColor(),
                TextColor::Secondary => NSColor::secondaryLabelColor(),
            };
            field.setTextColor(Some(&ns_color));
            control.text_color = Some(color);
        }
        self
    }

    /// Where the view sits across a stack view: along a vertical stack's
    /// width, or a horizontal stack's height. Buttons start at
    /// [`Align::Start`], every other view at [`Align::Stretch`]. Has no
    /// effect outside a stack view or box view.
    pub fn align(&mut self, align: Align) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if control.align != align {
            control.align = align;
            control.cross_stale = true;
        }
        self
    }

    /// Sets the view's width in points.
    pub fn width(&mut self, width: f64) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        match &control.width {
            Some(constraint) if constraint.constant() == width => {}
            Some(constraint) => {
                crate::write_check::record(self.node);
                constraint.setConstant(width);
            }
            None => {
                let constraint = control.view.widthAnchor().constraintEqualToConstant(width);
                constraint.setActive(true);
                control.width = Some(constraint);
            }
        }
        self
    }

    /// Sets the space inside a stack view's or box view's edges, in points:
    /// left, top, right, bottom.
    ///
    /// Panics on any other view.
    pub fn padding(&mut self, padding: [f64; 4]) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if must_write(self.node, false, &control.padding, &Some(padding)) {
            let stack = control
                .control
                .arranged()
                .expect("only stack views and box views take padding")
                .clone();
            let [left, top, right, bottom] = padding;
            stack.setEdgeInsets(NSEdgeInsets {
                top,
                left,
                bottom,
                right,
            });
            control.padding = Some(padding);
            // The children's cross-axis constraints carry the insets. They
            // are rebuilt now, because the stack may already be closed.
            for child in control.visual.clone() {
                self.ui.control_mut(child).cross_stale = true;
            }
            self.ui.place_across(self.node, &stack);
        }
        self
    }

    /// Whether the keyboard focus left the text field after an edit since
    /// the last pass.
    pub fn did_end_editing(&self) -> bool {
        self.ui.events.happened(self.node, EventKind::DidEndEditing)
    }
}

/// Where a view sits across its stack view, for [`Response::align`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// A system text color for [`Response::text_color`], which follows the
/// appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextColor {
    Label,
    /// For text under a title, such as a subtitle.
    Secondary,
}

/// A text style for [`Response::style`], mapped to the system's preferred
/// font for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// 22 points.
    Title,
    /// 17 points.
    Title2,
    /// [`Title2`](Self::Title2) in bold, for a window's heading.
    Heading,
    Body,
    Caption,
}

/// Whether a managed value of `node` must be written: the control is
/// stale, or the declared value differs from the last one written. A true
/// answer counts as a write for the idle pass check, at the caller's
/// location.
#[track_caller]
fn must_write<T: ?Sized + PartialEq>(node: NodeId, stale: bool, written: &T, declared: &T) -> bool {
    let write = stale || written != declared;
    if write {
        crate::write_check::record(node);
    }
    write
}

/// The concrete view behind a node, with the values this layer last wrote.
pub(crate) enum Control {
    Stack {
        stack: Retained<NSStackView>,
        orientation: Option<Orientation>,
        /// `NaN` until first written.
        spacing: f64,
    },
    /// Contract: `constraints` are active and place the grid's children by
    /// `rows` while `placed` is true, and `stretched` lists exactly the
    /// children of star rows. Writing `rows` clears `placed`, and the grid's
    /// close rebuilds both.
    Grid {
        container: Retained<NSView>,
        rows: Vec<Length>,
        constraints: Vec<Retained<NSLayoutConstraint>>,
        placed: bool,
        /// Children whose vertical hugging the grid lowered, with the
        /// priority each had before, restored when the grid places again.
        stretched: Vec<(NodeId, f32)>,
    },
    Label {
        field: Retained<NSTextField>,
        text: String,
    },
    Button {
        button: Retained<NSButton>,
        label: String,
        /// [`None`] after an invalidate, until written again: the options
        /// run after the body has taken `stale`, so they cannot read it.
        /// The key equivalent, `""` for none. A button has one, so
        /// [`default_button`](ButtonResponse::default_button) and
        /// [`cancel_button`](ButtonResponse::cancel_button) replace each
        /// other's.
        key: Option<&'static str>,
        /// The spinner shown while busy, made on first use.
        spinner: Option<Retained<NSProgressIndicator>>,
        /// Like `key`.
        busy: Option<bool>,
    },
    TextField {
        field: Retained<NSTextField>,
        /// The field's text as last written or read back.
        text: String,
        placeholder: String,
    },
    SecureTextField {
        field: Retained<NSSecureTextField>,
        placeholder: String,
    },
    CheckBox {
        button: Retained<NSButton>,
        label: String,
        /// The state last written or read back.
        on: bool,
    },
    /// The node's view is the field stack around a row of the boxes.
    CodeBoxes {
        boxes: [Retained<NSTextField>; CODE_LENGTH],
        /// The digits last written or read back.
        digits: CodeDigits,
    },
    PopUpButton {
        button: Retained<NSPopUpButton>,
        items: Vec<String>,
        /// Which items the user can pick, as last written; empty after the
        /// items change, until written again.
        enabled_items: Vec<bool>,
        /// The index last written or read back; -1 for no selection.
        selected: isize,
    },
    ComboBox {
        combo_box: Retained<NSComboBox>,
        items: Vec<String>,
        /// The text last written or read back.
        text: String,
    },
    /// The node's view is the line's row.
    AlertLine { line: ErrorLine },
    /// The box itself is the node's view.
    BoxView { stack: Retained<NSStackView> },
    /// From [`Ui::native`]: only the view is known.
    Native,
    /// The node's view is the scroll view around `table`.
    TableView {
        table: Retained<NSTableView>,
        data: Box<TableData>,
    },
    /// The cell view AppKit gets for one table row; the row's control is
    /// pinned inside it by `pins`.
    Row {
        shell: Retained<NSView>,
        content: Option<NodeId>,
        pins: Vec<Retained<NSLayoutConstraint>>,
    },
}

impl Control {
    /// The `NSControl` the options act on, which for a field is the control
    /// inside its stack.
    pub(crate) fn as_control(&self) -> Option<Retained<NSControl>> {
        Some(match self {
            Control::Label { field, .. } | Control::TextField { field, .. } => {
                Retained::clone(field).into_super()
            }
            Control::Button { button, .. } | Control::CheckBox { button, .. } => {
                Retained::clone(button).into_super()
            }
            Control::SecureTextField { field, .. } => {
                Retained::clone(field).into_super().into_super()
            }
            Control::PopUpButton { button, .. } => {
                Retained::clone(button).into_super().into_super()
            }
            Control::ComboBox { combo_box, .. } => {
                Retained::clone(combo_box).into_super().into_super()
            }
            Control::Stack { .. }
            | Control::CodeBoxes { .. }
            | Control::AlertLine { .. }
            | Control::Grid { .. }
            | Control::BoxView { .. }
            | Control::TableView { .. }
            | Control::Row { .. }
            | Control::Native => return None,
        })
    }

    /// The widget's name, as its `Ui` method spells it.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Control::Stack { .. } => "stack_view",
            Control::Grid { .. } => "grid",
            Control::Label { .. } => "label",
            Control::Button { .. } => "button",
            Control::CheckBox { .. } => "check_box",
            Control::CodeBoxes { .. } => "code_boxes",
            Control::TextField { .. } => "text_field",
            Control::SecureTextField { .. } => "secure_text_field",
            Control::PopUpButton { .. } => "pop_up_button",
            Control::ComboBox { .. } => "combo_box",
            Control::BoxView { .. } => "box_view",
            Control::AlertLine { .. } => "alert_line",
            Control::Native => "native",
            Control::TableView { .. } => "table_view",
            Control::Row { .. } => "row",
        }
    }

    /// The objects this control registered with [`Events`](crate::events::Events),
    /// to forget when the node is dropped.
    pub(crate) fn senders(&self) -> impl Iterator<Item = &AnyObject> {
        // One sender, or the code boxes' fields.
        let (one, boxes): (Option<&AnyObject>, &[Retained<NSTextField>]) = match self {
            Control::Button { button, .. } | Control::CheckBox { button, .. } => {
                (Some(button), &[])
            }
            Control::TextField { field, .. } => (Some(field), &[]),
            Control::SecureTextField { field, .. } => (Some(field), &[]),
            Control::PopUpButton { button, .. } => (Some(button), &[]),
            Control::ComboBox { combo_box, .. } => (Some(combo_box), &[]),
            Control::TableView { table, .. } => (Some(table), &[]),
            Control::CodeBoxes { boxes, .. } => (None, boxes),
            Control::Stack { .. }
            | Control::Grid { .. }
            | Control::Label { .. }
            | Control::AlertLine { .. }
            | Control::BoxView { .. }
            | Control::Native
            | Control::Row { .. } => (None, &[]),
        };
        one.into_iter()
            .chain(boxes.iter().map(|field| field.as_ref()))
    }

    /// The stack that arranges this control's children, for a stack view
    /// or a box view.
    pub(crate) fn arranged(&self) -> Option<&Retained<NSStackView>> {
        match self {
            Control::Stack { stack, .. } | Control::BoxView { stack, .. } => Some(stack),
            _ => None,
        }
    }
}
