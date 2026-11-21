//! An immediate-mode UI over the WinUI 3 retained tree.
//!
//! See `plans/winui-immediate.md` for the design.

// The crate is Windows only and empty on other targets.
#![cfg(windows)]

mod bindings;
mod children;
mod events;
mod mailbox;
mod pass;
mod runner;
mod startup;
mod tree;
mod widget;

pub use events::Key;
pub use mailbox::Handle;
pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{
    Align, AppBarButtonResponse, ButtonResponse, CODE_LENGTH, CheckBoxResponse, CodeBoxes,
    CodeDigits, ComboBoxResponse, ContentAlignment, FieldHandle, FontIconResponse,
    IconButtonResponse, LabelResponse, Length, LinkResponse, ListId, NavigationEntry,
    NavigationSelection, NavigationViewResponse, Orientation, Overflow, PasswordField,
    PasswordHandle, PressableResponse, Response, RowFn, SLIDE_DURATION, SettingsCard,
    SettingsCardResponse, Severity, Slide, TextBoxResponse, TitleBarResponse, WindowResponse,
};
/// The type [Ui::take_password] returns.
pub use zeroize::Zeroizing;
