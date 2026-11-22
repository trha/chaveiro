//! An immediate-mode UI over the AppKit retained view tree.
//!
//! A proof of concept that shares its tree, child diff and task nodes with
//! `winui-immediate` by copy, so the two crates can evolve apart.

// The crate is macOS only and empty on other targets.
#![cfg(target_os = "macos")]

mod children;
mod events;
mod pass;
mod runner;
mod tree;
mod widget;
mod write_check;

pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{
    Align, ButtonResponse, CODE_LENGTH, CheckBoxResponse, CodeBoxesResponse, CodeDigits,
    ComboBoxResponse, FieldHandle, Key, Length, MenuAnchor, MenuItemResponse, MenuResponse,
    Orientation, PopUpButtonResponse, Response, RowFn, SecureTextFieldHandle,
    SecureTextFieldResponse, SourceListEntries, SourceListResponse, StreamResponse, Style,
    TableViewId, TableViewResponse, TaskResponse, TextColor, TextFieldResponse,
    ToolbarItemResponse, ToolbarSearchResponse, WindowResponse,
};
/// The type [`Ui::take_password`] returns.
pub use zeroize::Zeroizing;
