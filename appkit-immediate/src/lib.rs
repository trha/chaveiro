//! An immediate-mode UI over the AppKit retained view tree.
//!
//! A proof of concept that shares its tree, child diff and mailbox with
//! `winui-immediate` by copy, so the two crates can evolve apart.

mod children;
mod events;
mod mailbox;
mod pass;
mod runner;
mod tree;
mod widget;

pub use mailbox::Handle;
pub use pass::Ui;
pub use runner::{App, run};
pub use widget::{ButtonResponse, Orientation, Response, WindowResponse};
