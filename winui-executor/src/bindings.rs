//! The generated `DispatcherQueue` bindings. `build.rs` writes them into
//! `OUT_DIR` from the members `bindings.txt` names.

#![allow(nonstandard_style, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
