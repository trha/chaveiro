//! The modules of Chaveiro for Windows, shared by the app in `main.rs` and
//! the examples.

// The crate is Windows only. On other targets it is empty.
#![cfg(windows)]

pub mod bindings;
pub mod conflict;
pub mod copy;
pub mod edit;
pub mod history;
pub mod io;
pub mod login;
pub mod route;
pub mod screen;
pub mod second_factor;
pub mod vault;
pub mod wipe;
