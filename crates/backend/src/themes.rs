//! User themes: one TOML file per theme in the `themes` folder next to
//! `config.toml`. The file name without `.toml` is the theme's id.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use serde::{Serialize, Serializer};

use crate::{Backend, BackendError, toml_file};

/// Ids of the built-in themes; files can't use them.
pub const NORDIC_DARK: &str = "nordic-dark";
pub const NORDIC_LIGHT: &str = "nordic-light";

/// A theme file: the theme it builds on and the colors it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeFile {
    /// Shown in pickers. Loading fills in the id when the file has none.
    pub name: String,
    /// The id of the theme this one changes: a built-in or another file.
    pub base: String,
    /// Token name and color text, as written.
    pub colors: Vec<(String, String)>,
    /// Problems that don't stop the theme from loading. Never saved.
    pub warnings: Vec<String>,
}

/// One file of the themes folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedTheme {
    pub id: String,
    /// The error says why the file can't be used at all.
    pub file: Result<ThemeFile, String>,
    /// When the file was last written, to notice changes made elsewhere.
    pub modified: Option<SystemTime>,
}

/// How saving treats a file that is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeWrite {
    /// A new theme: fails when a file with its id exists.
    Create,
    /// Replaces the file only if it is unchanged since it was read at
    /// `modified`. A deleted file is written again.
    Replace { modified: Option<SystemTime> },
    /// Replaces whatever is there.
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSaved {
    Saved {
        modified: Option<SystemTime>,
    },
    /// The file changed since it was read; nothing was written.
    ChangedOnDisk,
}

/// Reads a theme file's text. Unknown keys and colors that aren't strings
/// become warnings; bad TOML or a missing `base` is an error.
pub fn parse(text: &str) -> Result<ThemeFile, String> {
    let table: toml::Table = toml::from_str(text).map_err(|error| {
        let line = error
            .span()
            .map(|span| text[..span.start.min(text.len())].lines().count().max(1));
        match line {
            Some(line) => format!("line {line}: {}", error.message()),
            None => error.message().to_owned(),
        }
    })?;
    let mut file = ThemeFile::default();
    for (key, value) in table {
        match (key.as_str(), value) {
            ("name", toml::Value::String(name)) => file.name = name,
            ("base", toml::Value::String(base)) => file.base = base,
            ("colors", toml::Value::Table(colors)) => {
                for (token, color) in colors {
                    match color {
                        toml::Value::String(color) => file.colors.push((token, color)),
                        other => file
                            .warnings
                            .push(format!("colors.{token}: {other} is not a string")),
                    }
                }
            }
            ("name" | "colors", other) => {
                let kind = if key == "name" { "a string" } else { "a table" };
                file.warnings.push(format!("{key}: {other} is not {kind}"));
            }
            ("base", other) => return Err(format!("base: {other} is not a string")),
            (_, _) => file.warnings.push(format!("unknown key `{key}`")),
        }
    }
    if file.base.is_empty() {
        return Err(format!(
            "`base` is missing: name {NORDIC_DARK}, {NORDIC_LIGHT} or another theme"
        ));
    }
    Ok(file)
}

/// Every `*.toml` file in `dir`, by id. A missing folder has none.
fn list(dir: &Path) -> Result<Vec<LoadedTheme>, BackendError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(
                BackendError::new(format!("Could not read {}.", dir.display())).with_detail(error),
            );
        }
    };
    let mut themes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let file = fs::read_to_string(&path)
            .map_err(|error| format!("could not read: {error}"))
            .and_then(|text| parse(&text))
            .map(|mut file| {
                if file.name.trim().is_empty() {
                    id.clone_into(&mut file.name);
                }
                file
            });
        themes.push(LoadedTheme {
            id: id.to_owned(),
            file,
            modified: metadata.modified().ok(),
        });
    }
    themes.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(themes)
}

#[derive(Serialize)]
struct Written<'a> {
    name: &'a str,
    base: &'a str,
    colors: Ordered<'a>,
}

/// Colors in the order given, not sorted.
struct Ordered<'a>(&'a [(String, String)]);

impl Serialize for Ordered<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(token, color)| (token, color)))
    }
}

fn path_of(dir: &Path, id: &str) -> Result<std::path::PathBuf, BackendError> {
    let plain = !id.is_empty()
        && !id.starts_with('.')
        && !id.contains(['/', '\\', ':'])
        && id != NORDIC_DARK
        && id != NORDIC_LIGHT;
    if !plain {
        return Err(BackendError::new(format!(
            "\"{id}\" can't be a theme file name."
        )));
    }
    Ok(dir.join(format!("{id}.toml")))
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn save(
    dir: &Path,
    id: &str,
    file: &ThemeFile,
    write: ThemeWrite,
) -> Result<ThemeSaved, BackendError> {
    let path = path_of(dir, id)?;
    let exists = path.try_exists().unwrap_or(true);
    match write {
        ThemeWrite::Create if exists => {
            return Err(BackendError::new(format!(
                "A theme file named {id}.toml already exists. Reload themes to see it."
            )));
        }
        ThemeWrite::Replace { modified: read } if exists && modified(&path) != read => {
            return Ok(ThemeSaved::ChangedOnDisk);
        }
        _ => {}
    }
    let written = Written {
        name: &file.name,
        base: &file.base,
        colors: Ordered(&file.colors),
    };
    toml_file::write(&path, &written)?;
    Ok(ThemeSaved::Saved {
        modified: modified(&path),
    })
}

impl Backend {
    pub fn themes_dir(&self) -> &Path {
        &self.themes_dir
    }

    /// Reads the themes right away. The app calls this once at startup,
    /// before the first frame, so the window opens in the right colors.
    pub fn load_themes(&self) -> Result<Vec<LoadedTheme>, BackendError> {
        list(&self.themes_dir)
    }

    pub(crate) async fn reload_themes(&self) -> Result<Vec<LoadedTheme>, BackendError> {
        let dir = self.themes_dir.clone();
        crate::blocking(move || list(&dir)).await
    }

    pub(crate) async fn save_theme(
        &self,
        id: String,
        file: ThemeFile,
        write: ThemeWrite,
    ) -> Result<ThemeSaved, BackendError> {
        let dir = self.themes_dir.clone();
        crate::blocking(move || save(&dir, &id, &file, write)).await
    }

    /// Shows the themes folder in the file manager, creating it first.
    pub(crate) async fn open_themes_folder(&self) -> Result<(), BackendError> {
        let dir = self.themes_dir.clone();
        let created = dir.clone();
        crate::blocking(move || {
            fs::create_dir_all(&created).map_err(|error| {
                BackendError::new(format!("Could not create {}.", created.display()))
                    .with_detail(error)
            })
        })
        .await?;
        let program = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let mut child = tokio::process::Command::new(program)
            .arg(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| {
                BackendError::new(format!("Could not open {}.", dir.display())).with_detail(error)
            })?;
        // Explorer exits with 1 even when it worked, so the status says nothing.
        let _ = child.wait().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nordpil-themes-{}", Uuid::new_v4()))
    }

    fn ocean() -> ThemeFile {
        ThemeFile {
            name: "Ocean \"deep\"".to_owned(),
            base: NORDIC_DARK.to_owned(),
            colors: vec![
                ("main".to_owned(), "#10141c".to_owned()),
                ("accent".to_owned(), "#5b9be0".to_owned()),
            ],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn saved_files_load_back_with_colors_in_order() {
        let dir = temp_dir();
        assert_eq!(list(&dir).unwrap(), Vec::new());

        let saved = save(&dir, "ocean", &ocean(), ThemeWrite::Create).unwrap();
        let ThemeSaved::Saved { modified } = saved else {
            panic!("not saved: {saved:?}");
        };
        let text = fs::read_to_string(dir.join("ocean.toml")).unwrap();
        assert!(
            text.find("main =").unwrap() < text.find("accent =").unwrap(),
            "{text}"
        );

        let loaded = list(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "ocean");
        assert_eq!(loaded[0].modified, modified);
        let mut file = loaded[0].file.clone().unwrap();
        file.colors.sort();
        let mut expected = ocean();
        expected.colors.sort();
        assert_eq!(file, expected);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_file_is_an_error_for_that_file_only() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("good.toml"), "base = \"nordic-light\"\n").unwrap();
        fs::write(dir.join("bad.toml"), "base = \"nordic-dark\"\n[colors\n").unwrap();
        fs::write(dir.join("notes.txt"), "not a theme").unwrap();

        let loaded = list(&dir).unwrap();
        let ids: Vec<_> = loaded.iter().map(|theme| theme.id.as_str()).collect();
        assert_eq!(ids, ["bad", "good"]);
        let error = loaded[0].file.as_ref().unwrap_err();
        assert!(error.starts_with("line 2:"), "{error}");
        let good = loaded[1].file.as_ref().unwrap();
        assert_eq!(good.name, "good");
        assert_eq!(good.base, NORDIC_LIGHT);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn odd_keys_and_values_warn_and_a_missing_base_fails() {
        let file =
            parse("base = \"nordic-dark\"\nextra = 1\n[colors]\naccent = 5\nmain = \"#000000\"\n")
                .unwrap();
        assert_eq!(file.colors, [("main".to_owned(), "#000000".to_owned())]);
        assert_eq!(
            file.warnings,
            ["colors.accent: 5 is not a string", "unknown key `extra`"]
        );

        let error = parse("name = \"No base\"\n").unwrap_err();
        assert!(error.starts_with("`base` is missing"), "{error}");
    }

    #[test]
    fn saving_refuses_to_clobber() {
        let dir = temp_dir();
        let ThemeSaved::Saved { modified } =
            save(&dir, "ocean", &ocean(), ThemeWrite::Create).unwrap()
        else {
            panic!("not saved");
        };
        assert!(save(&dir, "ocean", &ocean(), ThemeWrite::Create).is_err());
        assert!(save(&dir, "nordic-dark", &ocean(), ThemeWrite::Overwrite).is_err());
        assert!(save(&dir, "../ocean", &ocean(), ThemeWrite::Overwrite).is_err());

        let unchanged = ThemeWrite::Replace { modified };
        assert!(matches!(
            save(&dir, "ocean", &ocean(), unchanged).unwrap(),
            ThemeSaved::Saved { .. }
        ));
        let stale = ThemeWrite::Replace {
            modified: Some(SystemTime::UNIX_EPOCH),
        };
        assert_eq!(
            save(&dir, "ocean", &ocean(), stale).unwrap(),
            ThemeSaved::ChangedOnDisk
        );
        assert!(matches!(
            save(&dir, "ocean", &ocean(), ThemeWrite::Overwrite).unwrap(),
            ThemeSaved::Saved { .. }
        ));

        fs::remove_file(dir.join("ocean.toml")).unwrap();
        assert!(matches!(
            save(&dir, "ocean", &ocean(), stale).unwrap(),
            ThemeSaved::Saved { .. }
        ));
        let _ = fs::remove_dir_all(dir);
    }
}
