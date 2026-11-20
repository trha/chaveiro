//! Platform-neutral Bitwarden vault core. It does no I/O: the caller feeds it
//! bytes and runs the effects it asks for.

pub mod core;
pub mod crypto;
pub mod proto;
pub mod vault;
