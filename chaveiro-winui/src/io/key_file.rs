//! Reading an SSH private key from a file the user picks.

use crate::edit::{KeySource, KeyTextError};
use std::path::PathBuf;
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Controls::Dialogs::{
            GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
            OPENFILENAMEW,
        },
        WindowsAndMessaging::IsWindow,
    },
};
use zeroize::Zeroizing;

/// Reads the text the SSH key form asked for from `source`: the clipboard,
/// or a file the user picks in a dialog that belongs to `window`.
pub fn read(source: KeySource, window: Option<u64>) -> Result<Zeroizing<String>, KeyTextError> {
    match source {
        KeySource::Clipboard { .. } => super::clipboard::read().ok_or(KeyTextError::NoText),
        KeySource::File => pick(window),
    }
}

/// Shows the system's open dialog, starting in `~/.ssh`, and reads the file
/// the user picks. It blocks until the dialog closes. `window` is the
/// `AppWindow.Id` of the window the dialog belongs to.
pub fn pick(window: Option<u64>) -> Result<Zeroizing<String>, KeyTextError> {
    let path = picked(window).ok_or(KeyTextError::Cancelled)?;
    // An OpenSSH key, even RSA 16384 with a comment, is far below this.
    const LIMIT: u64 = 64 * 1024;
    if std::fs::metadata(&path).map_or(true, |metadata| metadata.len() > LIMIT) {
        return Err(KeyTextError::Unreadable);
    }
    let bytes = Zeroizing::new(std::fs::read(&path).map_err(|_| KeyTextError::Unreadable)?);
    // The bytes are copied once into the text, and both wipe on drop.
    std::str::from_utf8(&bytes)
        .map(|text| Zeroizing::new(text.to_owned()))
        .map_err(|_| KeyTextError::NotText)
}

/// The path the user picked, or [`None`] when they cancelled.
fn picked(window: Option<u64>) -> Option<PathBuf> {
    // The id of a top-level window's AppWindow is its HWND.
    let owner = window
        .map(|id| id as usize as HWND)
        // SAFETY: IsWindow takes any value.
        .filter(|&owner| unsafe { IsWindow(owner) } != 0)
        .unwrap_or(std::ptr::null_mut());
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    // Pairs of a label and a pattern, ended by an empty pair.
    let filter: Vec<u16> = "All files\0*\0\0"
        .encode_utf16()
        .collect();
    let title = wide("Import an SSH private key");
    let start = std::env::var_os("USERPROFILE")
        .map(|home| wide(&PathBuf::from(home).join(".ssh").to_string_lossy()));
    let mut file = vec![0u16; 32 * 1024];
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: start.as_ref().map_or(std::ptr::null(), |start| start.as_ptr()),
        lpstrTitle: title.as_ptr(),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR,
        // SAFETY: the rest of the struct is plain data, for which zero is
        // "not set".
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: every pointer in `dialog` outlives the call, and `file` holds
    // `nMaxFile` units.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let end = file.iter().position(|&unit| unit == 0).unwrap_or(file.len());
    Some(PathBuf::from(String::from_utf16_lossy(&file[..end])))
}
