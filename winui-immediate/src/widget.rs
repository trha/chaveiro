//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the control the first time, and
//! writes a managed property only when its value differs from the value this
//! layer last wrote, or when the app invalidated the control.

use crate::{
    bindings::{
        Button, ContentControl, ItemsRepeater, PasswordBox, RowDefinitionCollection, StackPanel,
        TextBlock, TextBox, UIElementCollection,
    },
    pass::Ui,
    tree::NodeId,
};
use windows_core::{IInspectable, Interface, Result};

mod button;
mod cache;
mod grid;
mod label;
mod list;
mod native;
mod password_box;
mod stack;
mod text_box;
mod window;

pub use button::ButtonResponse;
pub use grid::Length;
pub(crate) use list::index_source;
pub use list::{ListId, RowFn};
pub use password_box::{PasswordField, PasswordHandle};
pub use stack::Orientation;
pub use text_box::TextBoxResponse;
pub use window::WindowResponse;

/// Access to the control a widget declared, for what the widget does not
/// cover.
///
/// A property this crate manages (a label's text, a stack's spacing) is
/// written again only when its declared value changes. After writing one
/// imperatively, call [`invalidate`](Self::invalidate), or the next change
/// may be skipped because the layer still holds the old value.
pub struct Response<'u> {
    pub(crate) ui: &'u mut Ui,
    pub(crate) node: NodeId,
}

impl Response<'_> {
    /// The control. Cast it to the interface you need with your own WinUI
    /// bindings, or use [`cast`](Self::cast).
    pub fn element(&self) -> IInspectable {
        self.ui.control(self.node).element.clone().into()
    }

    /// The control as another interface, such as its concrete type in your
    /// own WinUI bindings.
    pub fn cast<T: Interface>(&self) -> Result<T> {
        self.ui.control(self.node).element.cast()
    }

    /// Makes the next pass write every managed property of this control.
    pub fn invalidate(&mut self) {
        self.ui.control_mut(self.node).stale = true;
    }
}

/// The concrete control behind a node, with the values this layer last wrote.
pub(crate) enum Control {
    Stack {
        panel: StackPanel,
        children: UIElementCollection,
        orientation: Option<Orientation>,
        /// `NaN` until first written.
        spacing: f64,
    },
    Grid {
        children: UIElementCollection,
        definitions: RowDefinitionCollection,
        rows: Vec<Length>,
    },
    Label {
        block: TextBlock,
        text: String,
    },
    Button {
        button: Button,
        content: TextBlock,
        label: String,
    },
    TextBox {
        text_box: TextBox,
        /// The control's text as last written or read back.
        text: String,
        placeholder: String,
        /// Whether an IME composition is open; app writes wait until it ends.
        composing: bool,
    },
    PasswordBox {
        password_box: PasswordBox,
        placeholder: String,
    },
    /// From [`Ui::native`]: only the element is known.
    Native,
    List {
        repeater: ItemsRepeater,
        data: Box<list::ListData>,
    },
    /// The shell WinUI gets for one list row; the row's control is its
    /// content.
    Row {
        shell: ContentControl,
        content: Option<NodeId>,
    },
}

impl Control {
    /// The child collection of a panel, or [`None`] for other controls.
    pub(crate) fn children(&self) -> Option<&UIElementCollection> {
        match self {
            Control::Stack { children, .. } | Control::Grid { children, .. } => Some(children),
            _ => None,
        }
    }
}

/// Whether a managed value must be written: the control is stale, or the
/// declared value differs from the last one written.
fn must_write<T: ?Sized + PartialEq>(stale: bool, written: &T, declared: &T) -> bool {
    stale || written != declared
}
