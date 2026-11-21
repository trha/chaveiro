//! The plain settings file.

use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};
use uuid::Uuid;

pub const FILE: &str = "settings.json";

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
pub fn read(path: &Path) -> Result<Option<Settings>, SettingsError> {
    match fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SettingsError::Read(error)),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(SettingsError::Invalid),
    }
}

/// Writes `settings` to a temporary file next to `path`, flushes it, then
/// renames it over `path`, so a crash leaves the old file or the new one.
pub fn write(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}
