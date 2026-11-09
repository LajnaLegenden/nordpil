//! Turns theme files into palettes: follows each theme's base down to a
//! built-in, then lays the theme's colors on top.

use std::collections::HashMap;

use nordpil_backend::settings::ThemeSettings;
use nordpil_backend::themes::{LoadedTheme, NORDIC_DARK, NORDIC_LIGHT, ThemeFile};

use super::palette::{Colors, Rgba, TOKENS, Token, nordic_dark, nordic_light};

/// Bases nest at most this deep.
const MAX_DEPTH: usize = 16;

/// A theme's finished colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub colors: Colors,
    /// From the built-in at the bottom of its bases.
    pub dark: bool,
}

/// A built-in theme or a theme file.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// `None` for built-ins.
    pub file: Option<LoadedTheme>,
    /// The error says why the theme can't be used.
    pub resolved: Result<Resolved, String>,
    /// Lines of the file that were skipped.
    pub warnings: Vec<String>,
}

impl Entry {
    pub fn builtin(&self) -> bool {
        self.file.is_none()
    }

    /// Whether this theme can be shown in dark (or light) mode.
    pub fn usable_as(&self, dark: bool) -> bool {
        self.resolved
            .as_ref()
            .is_ok_and(|resolved| resolved.dark == dark)
    }
}

/// Every theme: the built-ins first, then the files by id.
#[derive(Debug, Clone)]
pub struct Themes {
    pub entries: Vec<Entry>,
}

/// What a dark or light slot shows, and why when not the chosen theme.
pub struct Slot {
    pub colors: Colors,
    pub problem: Option<String>,
}

pub fn builtin_id(dark: bool) -> &'static str {
    if dark { NORDIC_DARK } else { NORDIC_LIGHT }
}

pub fn builtin_name(dark: bool) -> &'static str {
    if dark {
        "Nordic frost dark"
    } else {
        "Nordic frost light"
    }
}

fn builtin_entry(dark: bool) -> Entry {
    let (id, colors) = if dark {
        (NORDIC_DARK, nordic_dark())
    } else {
        (NORDIC_LIGHT, nordic_light())
    };
    Entry {
        id: id.to_owned(),
        name: builtin_name(dark).to_owned(),
        file: None,
        resolved: Ok(Resolved { colors, dark }),
        warnings: Vec::new(),
    }
}

/// The token colors a theme sets. Unknown tokens and bad colors are
/// skipped with a warning.
pub fn overrides(
    colors: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Vec<(&'static Token, Rgba)> {
    let mut found = Vec::new();
    for (key, text) in colors {
        let Some(token) = Token::find(key) else {
            warnings.push(format!("colors.{key}: unknown token"));
            continue;
        };
        let Some(color) = Rgba::parse(text) else {
            warnings.push(format!(
                "colors.{key}: \"{text}\" is not #rrggbb or #rrggbbaa"
            ));
            continue;
        };
        found.push((token, color));
    }
    found
}

/// Lays `overrides` over `base`. A token that is another one at an
/// opacity (`accent_soft`) follows its parent when only the parent is set,
/// keeping the base's opacity.
pub fn apply(base: &Colors, overrides: &[(&'static Token, Rgba)]) -> Colors {
    let mut colors = *base;
    let set = |key: &str| overrides.iter().any(|(token, _)| token.key == key);
    for (token, color) in overrides {
        (token.set)(&mut colors, *color);
    }
    for token in TOKENS {
        let Some(parent) = token.parent.and_then(Token::find) else {
            continue;
        };
        if set(parent.key) && !set(token.key) {
            let alpha = (token.get)(base).alpha();
            let followed = (parent.get)(&colors).with_alpha(alpha);
            (token.set)(&mut colors, followed);
        }
    }
    colors
}

/// Why a base can't be resolved.
enum Problem {
    Missing,
    Broken,
    Cycle(String),
    TooDeep,
}

impl Problem {
    fn describe(self, base: &str) -> String {
        match self {
            Self::Missing => format!("base \"{base}\" was not found"),
            Self::Broken => format!("base \"{base}\" can't be used"),
            Self::Cycle(chain) => format!("base cycle: {chain}"),
            Self::TooDeep => format!("bases nest deeper than {MAX_DEPTH}"),
        }
    }
}

/// Theme files by id: `None` for files that can't be used at all.
type Files<'a> = HashMap<&'a str, Option<&'a ThemeFile>>;

/// Resolves the theme `id`. `chain` holds the themes building on it.
fn resolve(files: &Files<'_>, id: &str, chain: &mut Vec<String>) -> Result<Resolved, Problem> {
    for dark in [true, false] {
        let builtin = builtin_entry(dark);
        if builtin.id == id {
            return builtin.resolved.map_err(|_| Problem::Broken);
        }
    }
    if chain.iter().any(|seen| seen == id) {
        let mut names = chain.clone();
        names.push(id.to_owned());
        return Err(Problem::Cycle(names.join(" → ")));
    }
    if chain.len() > MAX_DEPTH {
        return Err(Problem::TooDeep);
    }
    let file = match files.get(id) {
        Some(Some(file)) => *file,
        Some(None) => return Err(Problem::Broken),
        None => return Err(Problem::Missing),
    };
    chain.push(id.to_owned());
    let base = resolve(files, &file.base, chain);
    chain.pop();
    let base = base.map_err(|problem| match problem {
        Problem::Missing | Problem::Broken => Problem::Broken,
        other => other,
    })?;
    let colors = apply(&base.colors, &overrides(&file.colors, &mut Vec::new()));
    Ok(Resolved {
        colors,
        dark: base.dark,
    })
}

/// Resolves a theme with this `id`, `base` and colors.
fn resolve_layer(
    files: &Files<'_>,
    id: &str,
    base: &str,
    overrides: &[(&'static Token, Rgba)],
) -> Result<Resolved, String> {
    let base_resolved =
        resolve(files, base, &mut vec![id.to_owned()]).map_err(|problem| problem.describe(base))?;
    Ok(Resolved {
        colors: apply(&base_resolved.colors, overrides),
        dark: base_resolved.dark,
    })
}

fn reserved(id: &str) -> bool {
    id == NORDIC_DARK || id == NORDIC_LIGHT
}

impl Themes {
    pub fn new(loaded: Vec<LoadedTheme>) -> Self {
        let files: Files<'_> = loaded
            .iter()
            .map(|theme| {
                let file = theme.file.as_ref().ok().filter(|_| !reserved(&theme.id));
                (theme.id.as_str(), file)
            })
            .collect();
        let mut entries = vec![builtin_entry(true), builtin_entry(false)];
        for theme in &loaded {
            let mut warnings = Vec::new();
            let (name, resolved) = match &theme.file {
                _ if reserved(&theme.id) => (
                    theme.id.clone(),
                    Err(format!(
                        "\"{}\" is a built-in theme's id; rename the file",
                        theme.id
                    )),
                ),
                Ok(file) => {
                    warnings.clone_from(&file.warnings);
                    let own = overrides(&file.colors, &mut warnings);
                    (
                        file.name.clone(),
                        resolve_layer(&files, &theme.id, &file.base, &own),
                    )
                }
                Err(error) => (theme.id.clone(), Err(error.clone())),
            };
            entries.push(Entry {
                id: theme.id.clone(),
                name,
                file: Some(theme.clone()),
                resolved,
                warnings,
            });
        }
        Self { entries }
    }

    /// The same themes with `theme` added or replaced, e.g. after saving it.
    pub fn with_file(&self, theme: LoadedTheme) -> Self {
        let mut files: Vec<LoadedTheme> = self
            .entries
            .iter()
            .filter_map(|entry| entry.file.clone())
            .filter(|file| file.id != theme.id)
            .collect();
        files.push(theme);
        files.sort_by(|a, b| a.id.cmp(&b.id));
        Self::new(files)
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn files(&self) -> Files<'_> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let loaded = entry.file.as_ref()?;
                let file = loaded.file.as_ref().ok().filter(|_| !reserved(&entry.id));
                Some((entry.id.as_str(), file))
            })
            .collect()
    }

    /// The colors for the dark (or light) slot of `settings`, falling back
    /// to the built-in.
    pub fn slot(&self, settings: &ThemeSettings, dark: bool) -> Slot {
        let id = if dark {
            &settings.dark
        } else {
            &settings.light
        };
        let kind = if dark { "Dark" } else { "Light" };
        let fallback = builtin_name(dark);
        let problem = match self.get(id).map(|entry| &entry.resolved) {
            Some(Ok(resolved)) if resolved.dark == dark => {
                return Slot {
                    colors: resolved.colors,
                    problem: None,
                };
            }
            Some(Ok(_)) => format!(
                "{kind} theme \"{id}\" is a {} theme; showing {fallback}.",
                if dark { "light" } else { "dark" }
            ),
            Some(Err(error)) => {
                format!("{kind} theme \"{id}\" can't be used ({error}); showing {fallback}.")
            }
            None => format!("{kind} theme \"{id}\" was not found; showing {fallback}."),
        };
        Slot {
            colors: if dark { nordic_dark() } else { nordic_light() },
            problem: Some(problem),
        }
    }

    /// Resolves an unsaved theme with this `id`, `base` and colors.
    pub fn resolve_draft(
        &self,
        id: &str,
        base: &str,
        overrides: &[(&'static Token, Rgba)],
    ) -> Result<Resolved, String> {
        resolve_layer(&self.files(), id, base, overrides)
    }

    /// Whether `id` is `ancestor` or builds on it, directly or not.
    pub fn descends_from(&self, id: &str, ancestor: &str) -> bool {
        let files = self.files();
        let mut current = id;
        for _ in 0..=MAX_DEPTH {
            if current == ancestor {
                return true;
            }
            let Some(Some(file)) = files.get(current) else {
                return false;
            };
            current = &file.base;
        }
        false
    }

    /// Lines saying which theme files have problems, for one notice.
    pub fn problems(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &self.entries {
            if let Err(error) = &entry.resolved {
                lines.push(format!("{}.toml: {error}", entry.id));
            }
            for warning in &entry.warnings {
                lines.push(format!("{}.toml: {warning}", entry.id));
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use nordpil_backend::settings::ThemeMode;

    use super::*;

    fn file(id: &str, base: &str, colors: &[(&str, &str)]) -> LoadedTheme {
        LoadedTheme {
            id: id.to_owned(),
            file: Ok(ThemeFile {
                name: id.to_uppercase(),
                base: base.to_owned(),
                colors: colors
                    .iter()
                    .map(|(key, color)| ((*key).to_owned(), (*color).to_owned()))
                    .collect(),
                warnings: Vec::new(),
            }),
            modified: None,
        }
    }

    fn colors(themes: &Themes, id: &str) -> Colors {
        themes.get(id).unwrap().resolved.as_ref().unwrap().colors
    }

    fn error(themes: &Themes, id: &str) -> String {
        themes.get(id).unwrap().resolved.clone().unwrap_err()
    }

    #[test]
    fn unset_tokens_inherit_from_the_base() {
        let themes = Themes::new(vec![file("ocean", NORDIC_DARK, &[("main", "#10141c")])]);
        let ocean = colors(&themes, "ocean");
        assert_eq!(ocean.main, Rgba::rgb(0x10141c));
        assert_eq!(
            Colors {
                main: nordic_dark().main,
                ..ocean
            },
            nordic_dark()
        );
        assert!(themes.get("ocean").unwrap().usable_as(true));
        assert!(!themes.get("ocean").unwrap().usable_as(false));
    }

    #[test]
    fn linked_tokens_follow_their_parent_with_the_base_opacity() {
        let themes = Themes::new(vec![file("paper", NORDIC_LIGHT, &[("accent", "#5b9be0")])]);
        let paper = colors(&themes, "paper");
        let light = nordic_light();
        assert_eq!(
            paper.accent_soft,
            Rgba::rgb(0x5b9be0).with_alpha(light.accent_soft.alpha())
        );
        assert_eq!(
            paper.focus,
            Rgba::rgb(0x5b9be0).with_alpha(light.focus.alpha())
        );
        assert_eq!(paper.danger_soft, light.danger_soft);
    }

    #[test]
    fn a_linked_token_set_explicitly_stays() {
        let themes = Themes::new(vec![file(
            "ocean",
            NORDIC_DARK,
            &[("accent", "#5b9be0"), ("accent_soft", "#ff000040")],
        )]);
        let ocean = colors(&themes, "ocean");
        assert_eq!(ocean.accent_soft, Rgba([255, 0, 0, 0x40]));
        assert_eq!(
            ocean.focus,
            Rgba::rgb(0x5b9be0).with_alpha(nordic_dark().focus.alpha())
        );
    }

    #[test]
    fn a_linked_token_set_lower_in_the_chain_is_kept_until_its_parent_changes() {
        let themes = Themes::new(vec![
            file("ocean", NORDIC_DARK, &[("accent_soft", "#ff000040")]),
            file("deep", "ocean", &[("main", "#000000")]),
            file("bright", "ocean", &[("accent", "#00ff00")]),
        ]);
        assert_eq!(colors(&themes, "deep").accent_soft, Rgba([255, 0, 0, 0x40]));
        assert_eq!(
            colors(&themes, "bright").accent_soft,
            Rgba([0, 255, 0, 0x40])
        );
    }

    #[test]
    fn bases_chain_and_the_kind_comes_from_the_built_in() {
        let themes = Themes::new(vec![
            file("a", NORDIC_LIGHT, &[("main", "#111111")]),
            file("b", "a", &[("side", "#222222")]),
            file("c", "b", &[("main", "#333333")]),
        ]);
        let c = colors(&themes, "c");
        assert_eq!(c.main, Rgba::rgb(0x333333));
        assert_eq!(c.side, Rgba::rgb(0x222222));
        assert_eq!(c.text, nordic_light().text);
        assert!(themes.get("c").unwrap().usable_as(false));
        assert!(themes.descends_from("c", "a"));
        assert!(!themes.descends_from("a", "c"));
    }

    #[test]
    fn cycles_missing_and_broken_bases_make_a_theme_unusable() {
        let mut broken = file("broken", NORDIC_DARK, &[]);
        broken.file = Err("line 2: expected `]`".to_owned());
        let themes = Themes::new(vec![
            file("a", "b", &[]),
            file("b", "a", &[]),
            file("self", "self", &[]),
            file("orphan", "nope", &[]),
            file("child", "orphan", &[]),
            file("on-broken", "broken", &[]),
            broken,
        ]);
        assert_eq!(error(&themes, "a"), "base cycle: a → b → a");
        assert_eq!(error(&themes, "self"), "base cycle: self → self");
        assert_eq!(error(&themes, "orphan"), "base \"nope\" was not found");
        assert_eq!(error(&themes, "child"), "base \"orphan\" can't be used");
        assert_eq!(error(&themes, "on-broken"), "base \"broken\" can't be used");
        assert_eq!(error(&themes, "broken"), "line 2: expected `]`");
    }

    #[test]
    fn deep_chains_stop() {
        let mut files = vec![file("t0", NORDIC_DARK, &[])];
        for i in 1..=MAX_DEPTH + 2 {
            files.push(file(&format!("t{i}"), &format!("t{}", i - 1), &[]));
        }
        let themes = Themes::new(files);
        assert!(themes.get("t3").unwrap().resolved.is_ok());
        let last = format!("t{}", MAX_DEPTH + 2);
        assert_eq!(
            error(&themes, &last),
            format!("bases nest deeper than {MAX_DEPTH}")
        );
    }

    #[test]
    fn bad_colors_and_unknown_tokens_warn_and_inherit() {
        let themes = Themes::new(vec![file(
            "ocean",
            NORDIC_DARK,
            &[
                ("acent", "#5b9be0"),
                ("main", "#12345"),
                ("side", "#000000"),
            ],
        )]);
        let entry = themes.get("ocean").unwrap();
        assert_eq!(
            entry.warnings,
            [
                "colors.acent: unknown token",
                "colors.main: \"#12345\" is not #rrggbb or #rrggbbaa"
            ]
        );
        let ocean = colors(&themes, "ocean");
        assert_eq!(ocean.main, nordic_dark().main);
        assert_eq!(ocean.side, Rgba::rgb(0));
        assert_eq!(themes.problems().len(), 2);
    }

    #[test]
    fn a_file_with_a_built_in_id_is_unusable() {
        let themes = Themes::new(vec![file(NORDIC_DARK, NORDIC_LIGHT, &[])]);
        assert_eq!(colors(&themes, NORDIC_DARK), nordic_dark());
        let file_entry = themes.entries.iter().find(|e| !e.builtin()).unwrap();
        assert!(
            file_entry
                .resolved
                .as_ref()
                .unwrap_err()
                .contains("built-in")
        );
    }

    #[test]
    fn slots_fall_back_to_the_built_in_and_say_why() {
        let themes = Themes::new(vec![
            file("paper", NORDIC_LIGHT, &[("main", "#ffffff")]),
            file("orphan", "nope", &[]),
        ]);
        let settings = |dark: &str, light: &str| ThemeSettings {
            mode: ThemeMode::System,
            dark: dark.to_owned(),
            light: light.to_owned(),
        };
        let ok = themes.slot(&settings(NORDIC_DARK, "paper"), false);
        assert_eq!(ok.colors.main, Rgba::rgb(0xffffff));
        assert_eq!(ok.problem, None);

        for (id, expected) in [
            ("paper", "is a light theme"),
            ("orphan", "can't be used"),
            ("gone", "was not found"),
        ] {
            let slot = themes.slot(&settings(id, NORDIC_LIGHT), true);
            assert_eq!(slot.colors, nordic_dark());
            let problem = slot.problem.unwrap();
            assert!(problem.contains(expected), "{problem}");
        }
    }

    #[test]
    fn drafts_resolve_against_the_loaded_files_and_refuse_cycles() {
        let themes = Themes::new(vec![
            file("ocean", NORDIC_DARK, &[]),
            file("deep", "ocean", &[]),
        ]);
        let accent = Token::find("accent").unwrap();
        let draft = themes
            .resolve_draft("new", "deep", &[(accent, Rgba::rgb(0xff0000))])
            .unwrap();
        assert!(draft.dark);
        assert_eq!(draft.colors.accent, Rgba::rgb(0xff0000));
        let error = themes.resolve_draft("ocean", "deep", &[]).unwrap_err();
        assert_eq!(error, "base cycle: ocean → deep → ocean");
    }

    #[test]
    fn replacing_a_file_updates_the_themes_built_on_it() {
        let themes = Themes::new(vec![
            file("ocean", NORDIC_DARK, &[("main", "#111111")]),
            file("deep", "ocean", &[]),
        ]);
        let themes = themes.with_file(file("ocean", NORDIC_DARK, &[("main", "#222222")]));
        assert_eq!(colors(&themes, "deep").main, Rgba::rgb(0x222222));
        let themes = themes.with_file(file("new", "deep", &[]));
        let ids: Vec<_> = themes.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, [NORDIC_DARK, NORDIC_LIGHT, "deep", "new", "ocean"]);
    }

    #[test]
    fn built_ins_survive_a_round_trip_through_a_theme_file() {
        for (id, builtin) in [(NORDIC_DARK, nordic_dark()), (NORDIC_LIGHT, nordic_light())] {
            let all: Vec<(String, String)> = TOKENS
                .iter()
                .map(|token| (token.key.to_owned(), (token.get)(&builtin).hex()))
                .collect();
            let other = if id == NORDIC_DARK {
                NORDIC_LIGHT
            } else {
                NORDIC_DARK
            };
            let themes = Themes::new(vec![LoadedTheme {
                id: "copy".to_owned(),
                file: Ok(ThemeFile {
                    name: "Copy".to_owned(),
                    base: other.to_owned(),
                    colors: all,
                    warnings: Vec::new(),
                }),
                modified: None,
            }]);
            assert_eq!(colors(&themes, "copy"), builtin);
        }
    }
}
