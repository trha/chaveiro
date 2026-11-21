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
        self.ui.control_mut(self.node).stale = true;
    }
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
    Label {
        field: Retained<NSTextField>,
        text: String,
    },
    Button {
        button: Retained<NSButton>,
        label: String,
    },
}
