//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the view the first time, and writes
//! a managed property only when its value differs from the value this layer
//! last wrote.

use crate::{pass::Ui, tree::NodeId};
use objc2::rc::Retained;
use objc2_app_kit::{NSButton, NSStackView, NSTextField, NSView};

mod button;
mod label;
mod stack_view;
mod window;

pub use button::ButtonResponse;
pub use stack_view::Orientation;
pub use window::WindowResponse;

/// Access to the view a widget declared, for what the widget does not cover.
///
/// A property this crate manages (a label's text, a stack's spacing) is
/// written again only when its declared value changes, so a value written
/// here may be overwritten or kept depending on later declarations.
pub struct Response<'u> {
    pub(crate) ui: &'u mut Ui,
    pub(crate) node: NodeId,
}

impl Response<'_> {
    /// The view. Downcast it with `objc2` to reach its concrete class.
    pub fn view(&self) -> Retained<NSView> {
        self.ui.control(self.node).view.clone()
    }
}

/// The concrete view behind a node, with the values this layer last wrote.
pub(crate) enum Control {
    Stack {
        stack: Retained<NSStackView>,
        orientation: Option<Orientation>,
        /// `NaN` until first written.
        spacing: f64,
    },
    Label {
        field: Retained<NSTextField>,
        text: String,
    },
    Button {
        button: Retained<NSButton>,
        label: String,
    },
}
