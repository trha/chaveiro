//! The Win32 clipboard, for secrets.

use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE, HWND},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
    },
    UI::WindowsAndMessaging::IsWindow,
};
use zeroize::Zeroizing;

/// `CF_UNICODETEXT`.
const UNICODE_TEXT: u32 = 13;

/// Names the clipboard content of one copy, so a later clear empties the
/// clipboard only while it still holds that copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied(u32);

/// Why [`copy`] wrote nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    /// Another app has the clipboard open; try again in a moment.
    Busy,
    /// A write failed; the clipboard may now be empty.
    Failed,
}

/// Puts `text` on the clipboard, marked so clipboard monitors, Win+V history
/// and cloud sync skip it. It never waits for the clipboard, but emptying it
/// tells the app that owned it, which a hung app answers late.
///
/// `window` is the `AppWindow.Id` of the window that owns the copy, as the
/// clipboard docs ask of a writer. [`None`], or an id that names no
/// window, copies with no owner, which works too.
pub fn copy(window: Option<u64>, text: &str) -> Result<Copied, CopyError> {
    // The id of a top-level window's AppWindow is its HWND.
    let owner = window
        .map(|id| id as usize as HWND)
        // SAFETY: IsWindow takes any value.
        .filter(|&owner| unsafe { IsWindow(owner) } != 0)
        .unwrap_or(std::ptr::null_mut());
    // SAFETY: `owner` is a live window or null.
    if unsafe { OpenClipboard(owner) } == 0 {
        return Err(CopyError::Busy);
    }
    // SAFETY: the clipboard is open on this thread.
    let written = unsafe { EmptyClipboard() != 0 } && write(text);
    // SAFETY: this thread opened the clipboard above.
    unsafe { CloseClipboard() };
    match written {
        // SAFETY: no preconditions.
        true => Ok(Copied(unsafe { GetClipboardSequenceNumber() })),
        false => Err(CopyError::Failed),
    }
}

/// Empties the clipboard if it still holds `copied`. A busy clipboard is
/// left as it is.
pub fn clear(copied: Copied) {
    // SAFETY: no preconditions.
    if unsafe { GetClipboardSequenceNumber() } != copied.0 {
        return;
    }
    // SAFETY: a null owner is allowed.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        tracing::warn!("the clipboard was busy, so the copy stays on it");
        return;
    }
    // Someone may have copied between the check and the open.
    // SAFETY: no preconditions.
    if unsafe { GetClipboardSequenceNumber() } == copied.0 {
        // Wipe the text first, because emptying frees its memory as it is.
        // SAFETY: the clipboard is open on this thread.
        let memory = unsafe { GetClipboardData(UNICODE_TEXT) };
        if !memory.is_null() {
            // SAFETY: the clipboard holds `memory` until it is emptied.
            let (pointer, size) = unsafe { (GlobalLock(memory), GlobalSize(memory)) };
            if !pointer.is_null() {
                // SAFETY: the block is `size` bytes and locked.
                unsafe { std::ptr::write_bytes(pointer.cast::<u8>(), 0, size) };
                // SAFETY: locked once above.
                unsafe { GlobalUnlock(memory) };
            }
        }
        // SAFETY: the clipboard is open on this thread.
        unsafe { EmptyClipboard() };
    }
    // SAFETY: this thread opened the clipboard above.
    unsafe { CloseClipboard() };
}

/// The text on the clipboard, or [`None`] when it holds no text or another
/// app has it open.
pub fn read() -> Option<Zeroizing<String>> {
    // SAFETY: a null owner is allowed.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return None;
    }
    // SAFETY: the clipboard is open on this thread.
    let memory = unsafe { GetClipboardData(UNICODE_TEXT) };
    let text = (!memory.is_null())
        .then(|| {
            // SAFETY: the clipboard holds `memory` until it closes.
            let (pointer, size) = unsafe { (GlobalLock(memory), GlobalSize(memory)) };
            (!pointer.is_null()).then(|| {
                // SAFETY: the block is `size` bytes and locked; the text
                // ends at its first NUL, or at the end of the block.
                let units = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2) };
                let end = units
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(units.len());
                let mut text = Zeroizing::new(String::with_capacity(end * 3));
                text.extend(
                    char::decode_utf16(units[..end].iter().copied())
                        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)),
                );
                // SAFETY: locked once above.
                unsafe { GlobalUnlock(memory) };
                text
            })
        })
        .flatten();
    // SAFETY: this thread opened the clipboard above.
    unsafe { CloseClipboard() };
    text
}

/// Sets the text and the three marks. The clipboard must be open and empty.
fn write(text: &str) -> bool {
    let units = text.encode_utf16().count() + 1;
    let Some(text_memory) = global(units * 2, |memory: *mut u16| {
        // The text goes straight into the clipboard's memory, so no other
        // copy of the secret is left to wipe.
        for (index, unit) in text.encode_utf16().chain([0]).enumerate() {
            // SAFETY: `memory` holds `units` units and `index < units`.
            unsafe { memory.add(index).write(unit) };
        }
    }) else {
        return false;
    };
    // SAFETY: the clipboard is open; on success it owns the memory.
    if unsafe { SetClipboardData(UNICODE_TEXT, text_memory) }.is_null() {
        // SAFETY: the clipboard refused the memory, so it is still ours.
        unsafe { GlobalFree(text_memory) };
        return false;
    }
    [
        // Its presence alone tells monitors and history to skip the content.
        "ExcludeClipboardContentFromMonitorProcessing",
        // A DWORD 0 in each keeps the content out of history and sync.
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ]
    .into_iter()
    .all(|name| {
        let name = name.encode_utf16().chain([0]).collect::<Vec<u16>>();
        // SAFETY: `name` ends in a NUL.
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        let Some(memory) = global(4, |memory: *mut u32| {
            // SAFETY: `memory` holds 4 bytes, aligned as GlobalAlloc aligns.
            unsafe { memory.write(0) }
        }) else {
            return false;
        };
        // SAFETY: as for the text above.
        let set = format != 0 && !unsafe { SetClipboardData(format, memory) }.is_null();
        if !set {
            // SAFETY: the clipboard refused the memory, so it is still ours.
            unsafe { GlobalFree(memory) };
        }
        set
    })
}

/// Movable global memory of `size` bytes, filled by `fill`.
fn global<T>(size: usize, fill: impl FnOnce(*mut T)) -> Option<HANDLE> {
    // SAFETY: no preconditions.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size) };
    if memory.is_null() {
        return None;
    }
    // SAFETY: `memory` is a live movable block.
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        // SAFETY: `memory` is ours and unlocked.
        unsafe { GlobalFree(memory) };
        return None;
    }
    fill(pointer.cast());
    // SAFETY: locked once above. It reports 0 once unlocked, which is fine.
    unsafe { GlobalUnlock(memory) };
    Some(memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::DataExchange::{GetClipboardOwner, IsClipboardFormatAvailable};

    fn available(name: &str) -> bool {
        let name = name.encode_utf16().chain([0]).collect::<Vec<u16>>();
        // SAFETY: `name` ends in a NUL.
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        // SAFETY: no preconditions.
        unsafe { IsClipboardFormatAvailable(format) != 0 }
    }

    #[test]
    #[ignore = "it replaces the user's clipboard"]
    fn a_copy_is_marked_and_its_clear_empties_the_clipboard() {
        let copied = copy(None, "chaveiro test").expect("the clipboard is free");
        // SAFETY: no preconditions.
        assert!(unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) } != 0);
        for name in [
            "ExcludeClipboardContentFromMonitorProcessing",
            "CanIncludeInClipboardHistory",
            "CanUploadToCloudClipboard",
        ] {
            assert!(available(name), "{name}");
        }
        clear(copied);
        // SAFETY: no preconditions.
        assert!(unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) } == 0);
    }

    #[test]
    #[ignore = "it replaces the user's clipboard"]
    fn a_window_owns_its_copy() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE,
        };
        let class = "STATIC".encode_utf16().chain([0]).collect::<Vec<u16>>();
        // SAFETY: `class` ends in a NUL; a message-only window needs no
        // other argument.
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null());
        let copied = copy(Some(window as usize as u64), "owned").expect("the clipboard is free");
        // SAFETY: no preconditions.
        assert_eq!(unsafe { GetClipboardOwner() }, window);
        clear(copied);
        // SAFETY: no preconditions.
        assert!(unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) } == 0);
        // SAFETY: this thread made the window.
        unsafe { DestroyWindow(window) };
    }

    #[test]
    #[ignore = "it replaces the user's clipboard"]
    fn a_clear_after_another_copy_keeps_that_copy() {
        let first = copy(None, "first").expect("the clipboard is free");
        copy(None, "second").expect("the clipboard is free");
        clear(first);
        // SAFETY: no preconditions.
        assert!(unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) } != 0);
    }
}
