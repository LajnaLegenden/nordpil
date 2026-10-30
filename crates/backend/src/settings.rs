//! User settings, kept in `config.toml` next to the saved connections.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Backend, BackendError, toml_file};

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follows the operating system.
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    Compact,
    #[default]
    Comfortable,
    Large,
}

/// What Ctrl+K searches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GotoScope {
    /// Every saved connection. Collection names are remembered on disk, and
    /// picking one on a closed connection connects to it first.
    #[default]
    AllSaved,
    /// Only connections that are open now; nothing is remembered.
    OpenOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub text_size: TextSize,
    pub goto_scope: GotoScope,
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(flatten)]
    settings: Settings,
}

/// Defaults when the file doesn't exist yet.
fn load(path: &Path) -> Result<Settings, BackendError> {
    let Some(file) = toml_file::read::<SettingsFile>(path)? else {
        return Ok(Settings::default());
    };
    if file.version > FILE_VERSION {
        return Err(toml_file::newer_version_error(path));
    }
    Ok(file.settings)
}

fn save(path: &Path, settings: Settings) -> Result<(), BackendError> {
    let file = SettingsFile {
        version: FILE_VERSION,
        settings,
    };
    toml_file::write(path, &file)
}

impl Backend {
    /// Reads the settings right away. The app calls this once at startup,
    /// before the first frame, so the window opens in the right theme.
    pub fn load_settings(&self) -> Result<Settings, BackendError> {
        load(&self.settings_path)
    }

    pub(crate) async fn save_settings(&self, settings: Settings) -> Result<(), BackendError> {
        let path = self.settings_path.clone();
        crate::blocking(move || save(&path, settings)).await
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn saves_loads_and_defaults() {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        let path = dir.join("config.toml");
        assert_eq!(load(&path).unwrap(), Settings::default());

        let settings = Settings {
            theme: Theme::Light,
            text_size: TextSize::Large,
            goto_scope: GotoScope::OpenOnly,
        };
        save(&path, settings).unwrap();

        assert_eq!(load(&path).unwrap(), settings);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("goto_scope = \"open-only\""), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
