//! An immediate-mode UI over the WinUI 3 retained tree.
//!
//! See `plans/winui-immediate.md` for the design.

mod bindings;
mod children;
mod events;
mod mailbox;
mod pass;
mod runner;
mod startup;
mod tree;
mod widget;

pub use mailbox::Handle;
pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{
    Align, ButtonResponse, ComboBoxResponse, FieldHandle, Length, ListId, Orientation,
    PasswordField, PasswordHandle, Response, RowFn, Severity, TextBoxResponse, WindowResponse,
};
/// The type [Ui::take_password] returns.
pub use zeroize::Zeroizing;
