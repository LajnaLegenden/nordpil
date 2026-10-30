//! Reading and atomically replacing the app's small TOML files.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::BackendError;

/// `None` when the file doesn't exist yet.
pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, BackendError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(path, "read", error)),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|error| file_error(path, "parse", error))
}

/// Replaces the file atomically, so a crash never leaves it half-written.
/// The file is only readable by the user.
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), BackendError> {
    let text =
        toml::to_string_pretty(value).map_err(|error| file_error(path, "serialize", error))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|error| file_error(path, "create the folder for", error))?;
    let temp = path.with_extension("toml.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options
        .open(&temp)
        .map_err(|error| file_error(path, "write", error))?;
    out.write_all(text.as_bytes())
        .and_then(|()| out.sync_all())
        .map_err(|error| file_error(path, "write", error))?;
    fs::rename(&temp, path).map_err(|error| file_error(path, "replace", error))
}

pub(crate) fn newer_version_error(path: &Path) -> BackendError {
    BackendError::new(format!(
        "{} was written by a newer version of nordpil.",
        path.display()
    ))
}

fn file_error(path: &Path, action: &str, error: impl std::fmt::Display) -> BackendError {
    BackendError::new(format!("Could not {action} {}.", path.display())).with_detail(error)
}
