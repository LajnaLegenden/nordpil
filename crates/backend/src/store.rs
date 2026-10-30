//! Saved connections, persisted as TOML in the platform config directory.
//! Passwords are never stored here; see [`crate::secrets`].

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::BackendError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: Uuid,
    pub name: String,
    /// Connection string without the password.
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ConnectionColor>,
    /// Blocks every write: inserts, updates, deletes, drops, index changes.
    #[serde(default)]
    pub read_only: bool,
}

/// Tags a connection, e.g. red for production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
}

impl ConnectionColor {
    pub const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Blue,
        Self::Purple,
        Self::Pink,
    ];
}

const FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ConnectionsFile {
    version: u32,
    #[serde(default, rename = "connection")]
    connections: Vec<SavedConnection>,
}

pub struct ConnectionStore {
    path: PathBuf,
}

impl ConnectionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `connections.toml` in the platform config directory.
    pub fn default_path() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "nordpil")?;
        Some(dirs.config_dir().join("connections.toml"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns no connections when the file doesn't exist yet.
    pub fn load(&self) -> Result<Vec<SavedConnection>, BackendError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(self.error("read", error)),
        };
        let file: ConnectionsFile =
            toml::from_str(&text).map_err(|error| self.error("parse", error))?;
        if file.version > FILE_VERSION {
            return Err(BackendError::new(format!(
                "{} was written by a newer version of nordpil.",
                self.path.display()
            )));
        }
        Ok(file.connections)
    }

    /// Replaces the file atomically, so a crash never leaves it half-written.
    pub fn save(&self, connections: &[SavedConnection]) -> Result<(), BackendError> {
        let file = ConnectionsFile {
            version: FILE_VERSION,
            connections: connections.to_vec(),
        };
        let text = toml::to_string_pretty(&file).map_err(|error| self.error("serialize", error))?;

        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|error| self.error("create the folder for", error))?;
        let temp = self.path.with_extension("toml.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut out = options
            .open(&temp)
            .map_err(|error| self.error("write", error))?;
        out.write_all(text.as_bytes())
            .and_then(|()| out.sync_all())
            .map_err(|error| self.error("write", error))?;
        fs::rename(&temp, &self.path).map_err(|error| self.error("replace", error))
    }

    fn error(&self, action: &str, error: impl std::fmt::Display) -> BackendError {
        BackendError::new(format!("Could not {action} {}.", self.path.display())).with_detail(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> ConnectionStore {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        ConnectionStore::new(dir.join("connections.toml"))
    }

    #[test]
    fn missing_file_loads_as_empty() {
        assert_eq!(temp_store().load().unwrap(), []);
    }

    #[test]
    fn saves_and_loads_connections() {
        let store = temp_store();
        let connections = vec![
            SavedConnection {
                id: Uuid::new_v4(),
                name: "local".to_owned(),
                uri: "mongodb://localhost".to_owned(),
                color: None,
                read_only: false,
            },
            SavedConnection {
                id: Uuid::new_v4(),
                name: "prod".to_owned(),
                uri: "mongodb+srv://admin@prod.example.net".to_owned(),
                color: Some(ConnectionColor::Red),
                read_only: true,
            },
        ];

        store.save(&connections).unwrap();

        assert_eq!(store.load().unwrap(), connections);
        let _ = fs::remove_dir_all(store.path().parent().unwrap());
    }

    #[test]
    fn rejects_files_from_newer_versions() {
        let store = temp_store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "version = 99\n").unwrap();

        assert!(store.load().is_err());
        let _ = fs::remove_dir_all(store.path().parent().unwrap());
    }
}
