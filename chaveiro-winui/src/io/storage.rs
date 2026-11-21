//! The files: the settings file, and one file per storage slot, DPAPI-sealed
//! when the slot is protected. Every function here blocks, so the queue in
//! [`Storage`](super::Storage) runs them on the thread pool.

use chaveiro_core::core::StoreSlot;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};
use uuid::Uuid;

/// The plain settings file, outside the account. The server is not here,
/// because the core keeps it with the account.
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Stable per install.
    pub device_id: Uuid,
}

impl Settings {
    /// Settings for a first start: a new device id.
    pub fn fresh() -> Self {
        Settings {
            device_id: Uuid::new_v4(),
        }
    }
}

/// Why the settings file did not load. A missing file is not an error.
#[derive(Debug)]
pub enum SettingsError {
    /// The read failed, such as an offline share or a denied access.
    Read(io::Error),
    /// The file is there but does not parse.
    Invalid(serde_json::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Read(error) => write!(formatter, "{error}"),
            SettingsError::Invalid(error) => write!(formatter, "the file does not parse: {error}"),
        }
    }
}

/// Reads the settings at `path`. A missing file gives [`None`].
pub fn read_settings(path: &Path) -> Result<Option<Settings>, SettingsError> {
    match fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SettingsError::Read(error)),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(SettingsError::Invalid),
    }
}
pub const SETTINGS: &str = "settings.json";

pub fn file_name(slot: StoreSlot) -> &'static str {
    match slot {
        StoreSlot::Account => "account.json",
        StoreSlot::RefreshToken => "refresh_token.bin",
        StoreSlot::VaultCache => "vault_cache.bin",
        StoreSlot::DeviceWrap => "device_wrap.bin",
        StoreSlot::TwoFactorToken => "two_factor_token.bin",
    }
}

pub fn load(folder: &Path, slot: StoreSlot) -> Option<Vec<u8>> {
    let bytes = fs::read(folder.join(file_name(slot))).ok()?;
    match slot.protected() {
        true => match unprotect(&bytes) {
            Ok(plain) => Some(plain),
            Err(error) => {
                tracing::warn!(%error, slot = file_name(slot), "unprotecting a slot failed");
                None
            }
        },
        false => Some(bytes),
    }
}

pub fn store(folder: &Path, slot: StoreSlot, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let path = folder.join(file_name(slot));
    match slot.protected() {
        true => write_atomic(&path, &protect(bytes)?),
        false => write_atomic(&path, bytes),
    }
}

/// Writes a temporary file next to `path`, flushes it to disk, then renames
/// it over `path`, so a crash leaves the old file or the new one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}
/// Seals `plain` with DPAPI for the current user.
fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(plain, true)
}

fn unprotect(sealed: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(sealed, false)
}

fn dpapi(input: &[u8], seal: bool) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let length = u32::try_from(input.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "slot larger than 4 GiB"))?;
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input_blob` points at `length` readable bytes, which DPAPI
    // only reads; the optional arguments are null; `output` is written by
    // the call.
    let succeeded = unsafe {
        match seal {
            true => CryptProtectData(
                &input_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            ),
            false => CryptUnprotectData(
                &input_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            ),
        }
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let size = output.cbData as usize;
    // SAFETY: on success DPAPI returns `cbData` bytes at `pbData`, allocated
    // with `LocalAlloc`; they are copied, wiped and freed here once.
    unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, size).to_vec();
        std::ptr::write_bytes(output.pbData, 0, size);
        LocalFree(output.pbData.cast());
        Ok(bytes)
    }
}
