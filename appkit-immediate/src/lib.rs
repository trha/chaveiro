//! An immediate-mode UI over the AppKit retained view tree.
//!
//! Shares its tree, caches, tasks and flows with `winui-immediate`
//! through `immediate-tree`.

// The crate is macOS only and empty on other targets.
#![cfg(target_os = "macos")]

mod events;
mod pass;
mod runner;
mod widget;

/// The shared tree, under the paths the widgets used before it moved.
mod tree {
    pub(crate) use immediate_tree::{Key, NodeId, Tree, Visit};
}
mod write_check;

pub use immediate_tree::lend;
pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{
    Align, ButtonResponse, CODE_LENGTH, CheckBoxResponse, CodeBoxesResponse, CodeDigits,
    ComboBoxResponse, FieldHandle, Flow, FlowResponse, Key, Lend, Length, Lent, MenuAnchor,
    MenuItemResponse, MenuResponse, Orientation, Owned, OwnedFamily, PopUpButtonResponse, Response,
    RowFn, SecureTextFieldHandle, SecureTextFieldResponse, SourceListEntries, SourceListResponse,
    StreamResponse, Style, TableViewId, TableViewResponse, TaskResponse, TextColor,
    TextFieldResponse, ToolbarItemResponse, ToolbarSearchResponse, WindowResponse, owned,
};
/// The type [`Ui::take_password`] returns.
pub use zeroize::Zeroizing;
