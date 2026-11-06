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

/// What nordpil does when a newer release is published on GitHub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Updates {
    /// Downloads and installs it, then offers a restart.
    #[default]
    Automatic,
    /// Says it is available and installs it on request.
    Notify,
    /// Never checks.
    Off,
}

/// What a collection tab shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    #[default]
    Documents,
    Indexes,
    Shell,
    Aggregate,
}

/// Where the shell's results sit relative to its script.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellOrientation {
    /// Script above, results below.
    #[default]
    Stacked,
    /// Script left, results right.
    SideBySide,
}

/// How the shell panel splits between script and results.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellLayout {
    pub orientation: ShellOrientation,
    /// The script's share of the height when stacked, from 0 to 1.
    pub stacked_split: f32,
    /// The script's share of the width side by side, from 0 to 1.
    pub side_split: f32,
}

impl ShellLayout {
    pub const STACKED_SPLIT: f32 = 0.4;
    pub const SIDE_SPLIT: f32 = 0.45;

    /// The script's share in the current orientation.
    pub fn split_mut(&mut self) -> &mut f32 {
        match self.orientation {
            ShellOrientation::Stacked => &mut self.stacked_split,
            ShellOrientation::SideBySide => &mut self.side_split,
        }
    }

    pub fn default_split(&self) -> f32 {
        match self.orientation {
            ShellOrientation::Stacked => Self::STACKED_SPLIT,
            ShellOrientation::SideBySide => Self::SIDE_SPLIT,
        }
    }
}

impl Default for ShellLayout {
    fn default() -> Self {
        Self {
            orientation: ShellOrientation::default(),
            stacked_split: Self::STACKED_SPLIT,
            side_split: Self::SIDE_SPLIT,
        }
    }
}

/// The aggregation panel's layout and previews.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AggregateSettings {
    /// The stages' share of the width, from 0 to 1.
    pub split: f32,
    /// Previews read at most this many documents while sampling is on.
    pub sample_size: u64,
}

impl AggregateSettings {
    pub const SPLIT: f32 = 0.45;
    pub const SAMPLE_SIZE: u64 = 100_000;
}

impl Default for AggregateSettings {
    fn default() -> Self {
        Self {
            split: Self::SPLIT,
            sample_size: Self::SAMPLE_SIZE,
        }
    }
}

/// The command-line agent that writes queries on request, if any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiAgent {
    #[default]
    Off,
    Claude,
    Codex,
    /// A program the user names, reading the prompt on stdin.
    Custom,
}

/// A built-in agent: Claude Code or Codex.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiPreset {
    /// The program; empty looks it up on the login shell's `PATH`.
    pub path: String,
    /// Passed as `--model`; empty uses the agent's fast tier
    /// ([`AiPreset::CLAUDE_MODEL`], [`AiPreset::CODEX_MODEL`]).
    pub model: String,
}

impl AiPreset {
    /// Claude Code's alias for its newest Haiku.
    pub const CLAUDE_MODEL: &str = "haiku";
    /// Codex's fast and affordable model.
    pub const CODEX_MODEL: &str = "gpt-6-luna";
}

/// A user-defined agent. It gets the prompt on stdin and prints a JSON
/// object; `{schema_file}` in an argument becomes the path of a file holding
/// the answer's JSON Schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiCustom {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub agent: AiAgent,
    pub claude: AiPreset,
    pub codex: AiPreset,
    pub custom: AiCustom,
    /// The agent is stopped after this long.
    pub timeout_secs: u64,
}

impl AiSettings {
    pub const TIMEOUT_SECS: u64 = 120;
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            agent: AiAgent::Off,
            claude: AiPreset::default(),
            codex: AiPreset::default(),
            custom: AiCustom::default(),
            timeout_secs: Self::TIMEOUT_SECS,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub text_size: TextSize,
    pub goto_scope: GotoScope,
    /// Right-clicking an id field like `userId` offers to open the document
    /// it refers to, in a collection of the same database like `users`.
    pub look_up_references: bool,
    pub updates: Updates,
    /// The panel the active tab showed last; new tabs open on it.
    pub last_panel: Panel,
    pub shell: ShellLayout,
    pub aggregate: AggregateSettings,
    pub ai: AiSettings,
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
            look_up_references: true,
            updates: Updates::Notify,
            last_panel: Panel::Shell,
            shell: ShellLayout {
                orientation: ShellOrientation::SideBySide,
                stacked_split: 0.25,
                side_split: 0.75,
            },
            aggregate: AggregateSettings {
                split: 0.3,
                sample_size: 500,
            },
            ai: AiSettings {
                agent: AiAgent::Custom,
                claude: AiPreset {
                    path: "/opt/claude".to_owned(),
                    model: "sonnet".to_owned(),
                },
                codex: AiPreset::default(),
                custom: AiCustom {
                    program: "ollama-query".to_owned(),
                    args: vec!["--schema".to_owned(), "{schema_file}".to_owned()],
                },
                timeout_secs: 30,
            },
        };
        save(&path, settings.clone()).unwrap();

        assert_eq!(load(&path).unwrap(), settings);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("goto_scope = \"open-only\""), "{text}");
        assert!(text.contains("look_up_references = true"), "{text}");
        assert!(text.contains("updates = \"notify\""), "{text}");
        assert!(text.contains("last_panel = \"shell\""), "{text}");
        assert!(text.contains("orientation = \"side-by-side\""), "{text}");
        assert!(text.contains("sample_size = 500"), "{text}");
        assert!(text.contains("agent = \"custom\""), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_from_before_the_shell_layout_get_defaults() {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "version = 1\ntheme = \"dark\"\n").unwrap();

        let settings = load(&path).unwrap();
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.last_panel, Panel::Documents);
        assert_eq!(settings.shell, ShellLayout::default());
        assert_eq!(settings.aggregate, AggregateSettings::default());
        assert_eq!(settings.ai, AiSettings::default());
        assert_eq!(settings.ai.agent, AiAgent::Off);
        let _ = std::fs::remove_dir_all(dir);
    }
}
