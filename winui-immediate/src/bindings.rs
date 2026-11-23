//! WinUI bindings: the generated file plus hand-written helpers that need its
//! private items.
//!
//! `build.rs` generates the bindings into `OUT_DIR` from the members
//! `bindings.txt` names.
//!
//! The generated event methods build a new delegate per call and capture a
//! closure, so this crate skips them: see [`crate::events::Delegate`].

#![allow(nonstandard_style, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use windows_core::Interface;
impl ITextBox {
    /// The text as the `HSTRING` WinUI returns, which the Windows string
    /// heap holds, so reading it allocates nothing on the Rust heap.
    pub(crate) fn text_string(&self) -> windows_core::Result<windows_core::HSTRING> {
        // SAFETY: the slot's signature is `HRESULT(this, *mut HSTRING)`; on
        // success the out value is an owned HSTRING.
        unsafe {
            let mut result = core::mem::zeroed();
            (Interface::vtable(self).Text)(Interface::as_raw(self), &mut result)
                .map(|| core::mem::transmute::<_, windows_core::HSTRING>(result))
        }
    }
}

impl IPasswordBox {
    /// The password as the `HSTRING` WinUI returns, unconverted, so the
    /// caller can wipe its buffer.
    pub(crate) fn password_string(&self) -> windows_core::Result<windows_core::HSTRING> {
        // SAFETY: the slot's signature is `HRESULT(this, *mut HSTRING)`; on
        // success the out value is an owned HSTRING.
        unsafe {
            let mut result = core::mem::zeroed();
            (Interface::vtable(self).Password)(Interface::as_raw(self), &mut result)
                .map(|| core::mem::transmute::<_, windows_core::HSTRING>(result))
        }
    }

    /// Sets the password from `value`, which the caller can wipe after.
    /// The generated setter takes `&str` and builds an `HSTRING` it frees
    /// unwiped.
    pub(crate) fn set_password_string(
        &self,
        value: &windows_core::HSTRING,
    ) -> windows_core::Result<()> {
        // SAFETY: the slot's signature is `HRESULT(this, HSTRING)`, and the
        // callee copies the borrowed string, as the generated setter relies
        // on.
        unsafe {
            (Interface::vtable(self).SetPassword)(
                Interface::as_raw(self),
                core::mem::transmute_copy(value),
            )
            .ok()
        }
    }
}
