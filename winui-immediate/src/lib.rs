//! An immediate-mode UI over the WinUI 3 retained tree.
//!
//! See `plans/winui-immediate.md` for the design.

// The crate is Windows only and empty on other targets.
#![cfg(windows)]

mod bindings;
#[cfg(feature = "count-calls")]
pub mod calls;
mod events;
mod pass;
mod runner;
mod startup;
mod theme;
mod widget;

/// The shared tree, under the paths the widgets used before it moved.
mod tree {
    pub(crate) use immediate_tree::{Key, NodeId, Tree, Visit};
}

pub use events::Key;
pub use immediate_tree::lend;
pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{
    Align, AppBarButtonResponse, ButtonResponse, CODE_LENGTH, CheckBoxResponse, CodeBoxes,
    CodeDigits, ComboBoxResponse, ContentAlignment, ContentButtonResponse, DialogAnswer,
    DialogButtons, DialogResponse, FieldHandle, Flow, FlowResponse, FontIconResponse,
    IconButtonResponse, LabelResponse, Lend, Length, Lent, LinkResponse, ListId, MORPH_DURATION,
    Mask, MenuAnchor, MenuItemResponse, MenuResponse, NativeResponse, NativeSender,
    NavigationEntry, NavigationSelection, NavigationViewResponse, Orientation, Overflow, Owned,
    OwnedFamily, PasswordField, PasswordHandle, PressableResponse, Response, RowFn, SLIDE_DURATION,
    SearchBoxResponse, SettingsCard, SettingsCardResponse, Severity, Slide, SplitButtonResponse,
    StreamResponse, TaskResponse, TextBoxResponse, TitleBarResponse, WindowResponse, owned,
};
/// The type [Ui::take_password] returns.
pub use zeroize::Zeroizing;
