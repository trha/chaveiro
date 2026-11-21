//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the view the first time, and writes
//! a managed property only when its value differs from the value this layer
//! last wrote.

use crate::{events::EventKind, pass::Ui, tree::NodeId};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSButton, NSComboBox, NSControl, NSFont, NSFontTextStyleBody, NSFontTextStyleCaption1,
    NSFontTextStyleTitle1, NSLayoutConstraint, NSPopUpButton, NSProgressIndicator,
    NSSecureTextField, NSStackView, NSTextField, NSView,
};
use objc2_foundation::{NSDictionary, NSEdgeInsets};

mod button;
mod combo_box;
mod field;
mod grid;
mod label;
mod pop_up_button;
mod secure_text_field;
mod stack_view;
mod text_field;
mod window;

pub use button::ButtonResponse;
pub use combo_box::ComboBoxResponse;
pub(crate) use field::Field;
pub use field::FieldHandle;
pub use grid::Length;
pub(crate) use grid::{Rule as GridRule, constraints as grid_constraints, rules as grid_rules};
pub use pop_up_button::PopUpButtonResponse;
pub use secure_text_field::{SecureTextFieldHandle, SecureTextFieldResponse};
pub use stack_view::Orientation;
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
        let control = self.ui.control_mut(self.node);
        control.stale = true;
        control.enabled = None;
        control.style = None;
        control.padding = None;
    }

    /// Sets whether the user can interact with the control.
    ///
    /// Panics when the view is not an `NSControl`, such as a stack view.
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if control.enabled != Some(on) {
            control
                .control
                .as_control()
                .expect("only controls can be disabled")
                .setEnabled(on);
            control.enabled = Some(on);
        }
        self
    }

    /// Sets the font of a label, text field or button to a text style.
    ///
    /// Panics when the view is not an `NSControl`.
    pub fn style(&mut self, style: Style) -> &mut Self {
        let control = self.ui.control_mut(self.node);
        if control.style != Some(style) {
            let text_style = match style {
                Style::Title => unsafe { NSFontTextStyleTitle1 },
                Style::Body => unsafe { NSFontTextStyleBody },
                Style::Caption => unsafe { NSFontTextStyleCaption1 },
            };
            // An empty options dictionary asks for the default font.
            let font = unsafe {
                NSFont::preferredFontForTextStyle_options(text_style, &NSDictionary::new())
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
            Some(constraint) => constraint.setConstant(width),
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
        if control.padding != Some(padding) {
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
            // The children's cross-axis constraints carry the insets.
            for child in control.visual.clone() {
                self.ui.control_mut(child).cross_stale = true;
            }
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

/// A text style for [`Response::style`], mapped to the system's preferred
/// font for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Title,
    Body,
    Caption,
}

/// Whether a managed value must be written: the control is stale, or the
/// declared value differs from the last one written.
fn must_write<T: ?Sized + PartialEq>(stale: bool, written: &T, declared: &T) -> bool {
    stale || written != declared
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
        default: bool,
        /// The spinner shown while busy, made on first use.
        spinner: Option<Retained<NSProgressIndicator>>,
        busy: bool,
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
    PopUpButton {
        button: Retained<NSPopUpButton>,
        items: Vec<String>,
        /// The index last written or read back; -1 for no selection.
        selected: isize,
    },
    ComboBox {
        combo_box: Retained<NSComboBox>,
        items: Vec<String>,
        /// The text last written or read back.
        text: String,
    },
    /// The box itself is the node's view.
    BoxView { stack: Retained<NSStackView> },
}

impl Control {
    /// The `NSControl` the options act on, which for a field is the control
    /// inside its stack.
    pub(crate) fn as_control(&self) -> Option<Retained<NSControl>> {
        Some(match self {
            Control::Label { field, .. } | Control::TextField { field, .. } => {
                Retained::clone(field).into_super()
            }
            Control::Button { button, .. } => Retained::clone(button).into_super(),
            Control::SecureTextField { field, .. } => {
                Retained::clone(field).into_super().into_super()
            }
            Control::PopUpButton { button, .. } => {
                Retained::clone(button).into_super().into_super()
            }
            Control::ComboBox { combo_box, .. } => {
                Retained::clone(combo_box).into_super().into_super()
            }
            Control::Stack { .. } | Control::Grid { .. } | Control::BoxView { .. } => return None,
        })
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
