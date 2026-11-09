//! The theme editor: a panel beside the app whose color changes show in the
//! app at once, and the dialog that starts a new theme.

use std::time::SystemTime;

use eframe::egui::{self, RichText, TextEdit, TextStyle};
use nordpil_backend::BackendError;
use nordpil_backend::themes::{ThemeFile, ThemeSaved, ThemeWrite};

use crate::icons;
use crate::theme::{self, Entry, Group, Resolved, Rgba, TOKENS, Themes, Token};
use crate::widgets::kit::{self, Tone};

/// A theme's name, base and the colors it sets.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    name: String,
    base: String,
    /// One per token, in `TOKENS` order; `None` inherits from the base.
    colors: Vec<Option<Rgba>>,
}

impl Draft {
    fn new(name: &str, base: &str) -> Self {
        Self {
            name: name.to_owned(),
            base: base.to_owned(),
            colors: vec![None; TOKENS.len()],
        }
    }

    fn from_file(file: &ThemeFile) -> Self {
        let mut draft = Self::new(&file.name, &file.base);
        for (token, color) in theme::overrides(&file.colors, &mut Vec::new()) {
            draft.colors[index(token)] = Some(color);
        }
        draft
    }

    fn overrides(&self) -> Vec<(&'static Token, Rgba)> {
        TOKENS
            .iter()
            .zip(&self.colors)
            .filter_map(|(token, color)| color.map(|color| (token, color)))
            .collect()
    }

    /// The file to save: only the colors this theme sets, in token order.
    fn file(&self, id: &str) -> ThemeFile {
        let name = self.name.trim();
        ThemeFile {
            name: if name.is_empty() { id } else { name }.to_owned(),
            base: self.base.clone(),
            colors: self
                .overrides()
                .into_iter()
                .map(|(token, color)| (token.key.to_owned(), color.hex()))
                .collect(),
            warnings: Vec::new(),
        }
    }
}

fn index(token: &Token) -> usize {
    TOKENS
        .iter()
        .position(|other| other.key == token.key)
        .unwrap_or_default()
}

/// A note over the colors about the file on disk.
#[derive(Debug, Clone, PartialEq)]
enum Banner {
    /// Reloading found the file changed while the draft has unsaved changes.
    ChangedOnDisk { modified: Option<SystemTime> },
    /// Saving found the file changed since it was read.
    SaveConflict,
    /// The file is gone or broken; saving writes it again.
    Rewrite(String),
}

pub enum Outcome {
    Open,
    /// The colors or the base changed; the app repaints with them.
    Changed,
    Save(ThemeWrite),
    /// Read the theme files again.
    Reload,
    OpenFolder,
    Close,
}

pub struct ThemeEditor {
    /// The file name without `.toml`.
    pub id: String,
    /// Not saved yet: the first save creates the file.
    pub new: bool,
    draft: Draft,
    saved: Draft,
    /// The draft being saved now.
    saving: Option<Draft>,
    /// When the file was read or last saved.
    modified: Option<SystemTime>,
    /// Lines of the file that were skipped; saving drops them.
    warnings: Vec<String>,
    banner: Option<Banner>,
    error: Option<String>,
    confirm_close: bool,
    close_after_save: bool,
    /// The next reload replaces the draft with the file.
    take_disk: bool,
    /// The hex field being typed in, by token, and its text.
    typing: Option<(usize, String)>,
}

impl ThemeEditor {
    pub fn create(id: String, name: &str, base: &str) -> Self {
        let draft = Draft::new(name, base);
        Self {
            id,
            new: true,
            saved: draft.clone(),
            draft,
            saving: None,
            modified: None,
            warnings: Vec::new(),
            banner: None,
            error: None,
            confirm_close: false,
            close_after_save: false,
            take_disk: false,
            typing: None,
        }
    }

    /// `None` for built-ins and files that can't be read.
    pub fn open(entry: &Entry) -> Option<Self> {
        let loaded = entry.file.as_ref()?;
        let file = loaded.file.as_ref().ok()?;
        let draft = Draft::from_file(file);
        Some(Self {
            id: entry.id.clone(),
            new: false,
            saved: draft.clone(),
            draft,
            modified: loaded.modified,
            warnings: entry.warnings.clone(),
            ..Self::create(String::new(), "", "")
        })
    }

    /// The draft's colors, if its base can be used.
    pub fn resolved(&self, themes: &Themes) -> Result<Resolved, String> {
        themes.resolve_draft(&self.id, &self.draft.base, &self.draft.overrides())
    }

    fn dirty(&self) -> bool {
        self.new || self.draft != self.saved
    }

    /// Whether the close confirmation is showing, a modal over the app.
    pub fn confirming(&self) -> bool {
        self.confirm_close
    }

    /// The file to send for saving; the answer goes to [`Self::saved`].
    pub fn start_save(&mut self) -> ThemeFile {
        self.error = None;
        self.saving = Some(self.draft.clone());
        self.draft.file(&self.id)
    }

    /// Takes the save's answer. Returns the saved file when it was written,
    /// and whether the editor should close now.
    pub fn saved(&mut self, result: Result<ThemeSaved, BackendError>) -> (Option<ThemeFile>, bool) {
        let saving = self.saving.take();
        match result {
            Ok(ThemeSaved::Saved { modified }) => {
                let saved = saving.unwrap_or_else(|| self.draft.clone());
                let file = saved.file(&self.id);
                self.saved = saved;
                self.new = false;
                self.modified = modified;
                self.banner = None;
                self.warnings.clear();
                (Some(file), self.close_after_save)
            }
            Ok(ThemeSaved::ChangedOnDisk) => {
                self.banner = Some(Banner::SaveConflict);
                self.close_after_save = false;
                (None, false)
            }
            Err(error) => {
                self.error = Some(error.message);
                self.close_after_save = false;
                (None, false)
            }
        }
    }

    /// When the file was last written, after a save.
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Follows the files after a reload: takes the file's colors when the
    /// draft has no unsaved changes, and says so when it has.
    pub fn themes_loaded(&mut self, themes: &Themes) {
        let take_disk = std::mem::take(&mut self.take_disk);
        if self.new {
            return;
        }
        let Some(loaded) = themes.get(&self.id).and_then(|entry| entry.file.as_ref()) else {
            self.modified = None;
            self.banner = Some(Banner::Rewrite(
                "The file is gone. Saving writes it again.".to_owned(),
            ));
            return;
        };
        match &loaded.file {
            Err(error) => {
                self.modified = loaded.modified;
                self.banner = Some(Banner::Rewrite(format!(
                    "The file can't be read ({error}). Saving replaces it."
                )));
            }
            Ok(file) => {
                let changed = loaded.modified != self.modified;
                if take_disk || (changed && !self.dirty()) {
                    self.draft = Draft::from_file(file);
                    self.saved = self.draft.clone();
                    self.modified = loaded.modified;
                    self.warnings = themes
                        .get(&self.id)
                        .map(|entry| entry.warnings.clone())
                        .unwrap_or_default();
                    self.banner = None;
                    self.typing = None;
                } else if changed {
                    self.banner = Some(Banner::ChangedOnDisk {
                        modified: loaded.modified,
                    });
                }
            }
        }
    }

    fn write(&self) -> ThemeWrite {
        if self.new {
            ThemeWrite::Create
        } else {
            ThemeWrite::Replace {
                modified: self.modified,
            }
        }
    }

    /// The panel's contents.
    pub fn show(&mut self, ui: &mut egui::Ui, themes: &Themes) -> Outcome {
        let palette = theme::palette(ui);
        let mut outcome = Outcome::Open;
        let resolved = self.resolved(themes);
        let inherited = themes
            .resolve_draft(&self.id, &self.draft.base, &[])
            .map(|base| base.colors);
        let dark = resolved.as_ref().map_or(ui.visuals().dark_mode, |r| r.dark);
        let fallback = if dark {
            theme::nordic_dark()
        } else {
            theme::nordic_light()
        };
        let colors = resolved.as_ref().map_or(fallback, |r| r.colors);
        let inherited = inherited.unwrap_or(fallback);

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Theme editor")
                    .text_style(theme::title_style())
                    .color(palette.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::icon_button(ui, icons::X, "Close theme editor", 28.0).clicked() {
                    if self.dirty() {
                        self.confirm_close = true;
                    } else {
                        outcome = Outcome::Close;
                    }
                }
            });
        });
        let file = if self.new {
            format!("New · saves as {}.toml", self.id)
        } else {
            format!("{}.toml", self.id)
        };
        let kind = if dark { "dark" } else { "light" };
        ui.label(
            RichText::new(format!("{file} · {kind}"))
                .monospace()
                .size(12.5)
                .color(palette.subtle),
        );
        ui.add_space(8.0);

        let label = ui.label("Name");
        ui.add(
            TextEdit::singleline(&mut self.draft.name)
                .hint_text(&self.id)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.add_space(4.0);
        ui.label("Based on");
        if self.base_picker(ui, themes) {
            outcome = Outcome::Changed;
        }
        if let Err(error) = &resolved {
            note(ui, icons::CIRCLE_ALERT, error, palette.danger);
        }
        if let Some(error) = &self.error {
            note(ui, icons::CIRCLE_ALERT, error, palette.danger);
        }
        if let Some(action) = self.banner(ui) {
            outcome = action;
        }
        if !self.warnings.is_empty() {
            let count = self.warnings.len();
            let text = format!(
                "{count} line{} of the file skipped; saving drops {}.",
                if count == 1 { "" } else { "s" },
                if count == 1 { "it" } else { "them" }
            );
            note(ui, icons::TRIANGLE_ALERT, &text, palette.warn)
                .on_hover_text(self.warnings.join("\n"));
        }
        ui.add_space(6.0);

        let footer = 64.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height((ui.available_height() - footer).max(80.0))
            .show(ui, |ui| {
                for group in Group::ALL {
                    ui.add_space(8.0);
                    kit::section_label(ui, group.label());
                    for (i, token) in TOKENS.iter().enumerate() {
                        if token.group == group && self.token_row(ui, i, &colors, &inherited) {
                            outcome = Outcome::Changed;
                        }
                    }
                }
                ui.add_space(8.0);
            });

        if let Some(action) = self.footer(ui) {
            outcome = action;
        }
        if self.confirm_close
            && let Some(action) = self.confirm(ui.ctx())
        {
            outcome = action;
        }
        outcome
    }

    /// Returns true when the base changed.
    fn base_picker(&mut self, ui: &mut egui::Ui, themes: &Themes) -> bool {
        let before = self.draft.base.clone();
        let text = themes
            .get(&self.draft.base)
            .map_or_else(|| self.draft.base.clone(), |entry| entry.name.clone());
        let combo = egui::ComboBox::from_id_salt("theme_base")
            .width(ui.available_width())
            .selected_text(text)
            .show_ui(ui, |ui| {
                for dark in [true, false] {
                    for entry in themes.entries.iter().filter(|entry| entry.usable_as(dark)) {
                        if themes.descends_from(&entry.id, &self.id) {
                            continue;
                        }
                        let kind = if dark { "dark" } else { "light" };
                        let label = format!("{} ({kind})", entry.name);
                        ui.selectable_value(&mut self.draft.base, entry.id.clone(), label);
                    }
                }
            });
        combo
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Based on"));
        self.draft.base != before
    }

    fn banner(&mut self, ui: &mut egui::Ui) -> Option<Outcome> {
        let palette = theme::palette(ui);
        let banner = self.banner.clone()?;
        let mut outcome = None;
        let text = match &banner {
            Banner::ChangedOnDisk { .. } => "The file changed on disk.",
            Banner::SaveConflict => "The file changed on disk since you opened it.",
            Banner::Rewrite(text) => text,
        };
        note(ui, icons::TRIANGLE_ALERT, text, palette.warn);
        ui.horizontal(|ui| {
            let height = 28.0;
            match banner {
                Banner::ChangedOnDisk { modified } => {
                    if kit::button(ui, Tone::Secondary, None, "Reload", height).clicked() {
                        self.take_disk = true;
                        outcome = Some(Outcome::Reload);
                    }
                    if kit::button(ui, Tone::Ghost, None, "Keep mine", height).clicked() {
                        self.modified = modified;
                        self.banner = None;
                    }
                }
                Banner::SaveConflict => {
                    if kit::button(ui, Tone::Secondary, None, "Overwrite", height).clicked() {
                        self.banner = None;
                        outcome = Some(Outcome::Save(ThemeWrite::Overwrite));
                    }
                    if kit::button(ui, Tone::Ghost, None, "Reload", height).clicked() {
                        self.take_disk = true;
                        self.banner = None;
                        outcome = Some(Outcome::Reload);
                    }
                }
                Banner::Rewrite(_) => {}
            }
        });
        outcome
    }

    /// One token: swatch, name, hex and reset. Returns true when it changed.
    fn token_row(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        colors: &theme::Colors,
        inherited: &theme::Colors,
    ) -> bool {
        let palette = theme::palette(ui);
        let token = &TOKENS[i];
        let set = self.draft.colors[i];
        let shown = (token.get)(colors);
        let mut changed = false;
        ui.horizontal(|ui| {
            let mut rgba = shown.0;
            if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
                self.draft.colors[i] = Some(Rgba(rgba));
                changed = true;
            }
            let color = if set.is_some() {
                palette.text
            } else {
                palette.muted
            };
            let mut name = RichText::new(token.key).monospace().size(12.5).color(color);
            if set.is_some() {
                name = name.strong();
            }
            ui.label(name);
            if let Some(parent) = token.parent.filter(|_| set.is_none()) {
                ui.label(
                    RichText::new(format!("from {parent}"))
                        .size(11.5)
                        .color(palette.subtle),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let reset = format!("Reset {} to the base", token.key);
                if set.is_some() {
                    if kit::icon_button(ui, icons::X, &reset, 24.0).clicked() {
                        self.draft.colors[i] = None;
                        changed = true;
                    }
                } else {
                    // Keeps the hex fields in one column.
                    ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                }
                let typing_here = self.typing.as_ref().is_some_and(|(row, _)| *row == i);
                let mut text = match self.typing.take_if(|_| typing_here) {
                    Some((_, text)) => text,
                    None => shown.hex(),
                };
                let response = ui.add(
                    TextEdit::singleline(&mut text)
                        .font(TextStyle::Monospace)
                        .desired_width(96.0),
                );
                let label = token.key;
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
                if response.has_focus() {
                    if response.changed()
                        && let Some(color) = Rgba::parse(&text)
                    {
                        self.draft.colors[i] = Some(color);
                        changed = true;
                    }
                    self.typing = Some((i, text));
                }
                let base = (token.get)(inherited);
                if set.is_some() && set != Some(base) {
                    response.on_hover_text(format!("Base: {}", base.hex()));
                }
            });
        });
        changed
    }

    fn footer(&mut self, ui: &mut egui::Ui) -> Option<Outcome> {
        let palette = theme::palette(ui);
        let mut outcome = None;
        ui.add_space(8.0);
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, palette.line),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let height = 32.0;
            if kit::icon_button(ui, icons::REFRESH, "Reload themes", height).clicked() {
                outcome = Some(Outcome::Reload);
            }
            if kit::icon_button(ui, icons::FOLDER_OPEN, "Open themes folder", height).clicked() {
                outcome = Some(Outcome::OpenFolder);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let saving = self.saving.is_some();
                let can_save =
                    !saving && (self.dirty() || matches!(self.banner, Some(Banner::Rewrite(_))));
                let label = if saving { "Saving…" } else { "Save" };
                let save = ui.add_enabled_ui(can_save, |ui| {
                    kit::button(ui, Tone::Primary, Some(icons::SAVE), label, height)
                });
                if save.inner.clicked() {
                    outcome = Some(Outcome::Save(self.write()));
                }
                let can_revert = self.draft != self.saved;
                let revert = ui.add_enabled_ui(can_revert, |ui| {
                    kit::button(ui, Tone::Ghost, None, "Revert", height)
                });
                if revert.inner.clicked() {
                    self.draft = self.saved.clone();
                    self.typing = None;
                    outcome = Some(Outcome::Changed);
                }
            });
        });
        ui.add_space(10.0);
        outcome
    }

    /// Asks what to do with unsaved changes when closing.
    fn confirm(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        let mut outcome = None;
        let name = self.draft.file(&self.id).name;
        let modal = kit::modal(ctx, egui::Id::unique("theme_editor_close")).show(ctx, |ui| {
            kit::dialog_width(ui, 420.0);
            kit::dialog_title(ui, &format!("Save changes to {name}?"), None);
            ui.label("The theme has changes that aren't saved.");
            kit::dialog_footer(ui, |ui| {
                if kit::button(ui, Tone::Primary, None, "Save", kit::DIALOG_HEIGHT).clicked() {
                    self.close_after_save = true;
                    self.confirm_close = false;
                    outcome = Some(Outcome::Save(self.write()));
                }
                if kit::button(ui, Tone::Danger, None, "Discard", kit::DIALOG_HEIGHT).clicked() {
                    outcome = Some(Outcome::Close);
                }
                if kit::button(ui, Tone::Ghost, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                    self.confirm_close = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_close = false;
        }
        outcome
    }
}

fn note(ui: &mut egui::Ui, icon: &str, text: &str, color: egui::Color32) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(format!("{icon} {text}"))
                .size(12.5)
                .color(color),
        )
        .wrap(),
    )
}

/// A free file name for a theme called `name`: `Ocean Deep` → `ocean-deep`,
/// then `ocean-deep-2` when taken.
pub fn new_id(name: &str, themes: &Themes) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "theme" } else { slug };
    if themes.get(slug).is_none() {
        return slug.to_owned();
    }
    (2..)
        .map(|n| format!("{slug}-{n}"))
        .find(|id| themes.get(id).is_none())
        .unwrap_or_else(|| slug.to_owned())
}

/// Starts a theme: its name and the theme it builds on.
pub struct NewThemeDialog {
    name: String,
    base: String,
}

pub enum NewOutcome {
    Open,
    Cancel,
    Create {
        id: String,
        name: String,
        base: String,
    },
}

impl NewThemeDialog {
    pub fn new(base: &str) -> Self {
        Self {
            name: String::new(),
            base: base.to_owned(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, themes: &Themes) -> NewOutcome {
        let mut outcome = NewOutcome::Open;
        let id = new_id(&self.name, themes);
        let modal = kit::modal(ctx, egui::Id::unique("new_theme")).show(ctx, |ui| {
            kit::dialog_width(ui, 440.0);
            kit::dialog_title(ui, "New theme", None);
            let label = ui.label("Name");
            let field = ui
                .add(
                    TextEdit::singleline(&mut self.name)
                        .hint_text("Ocean")
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id);
            if ui.memory(|m| m.focused().is_none()) {
                field.request_focus();
            }
            ui.label(
                RichText::new(format!("Saved as {id}.toml"))
                    .monospace()
                    .size(12.5)
                    .color(theme::palette(ui).subtle),
            );
            ui.add_space(8.0);
            ui.label("Based on");
            let text = themes
                .get(&self.base)
                .map_or_else(|| self.base.clone(), |entry| entry.name.clone());
            let combo = egui::ComboBox::from_id_salt("new_theme_base")
                .width(ui.available_width())
                .selected_text(text)
                .show_ui(ui, |ui| {
                    for dark in [true, false] {
                        for entry in themes.entries.iter().filter(|entry| entry.usable_as(dark)) {
                            let kind = if dark { "dark" } else { "light" };
                            let label = format!("{} ({kind})", entry.name);
                            ui.selectable_value(&mut self.base, entry.id.clone(), label);
                        }
                    }
                });
            combo
                .response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Based on"));
            let ready = !self.name.trim().is_empty();
            let enter =
                ready && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            kit::dialog_footer(ui, |ui| {
                let create = ui.add_enabled_ui(ready, |ui| {
                    kit::button(ui, Tone::Primary, None, "Create", kit::DIALOG_HEIGHT)
                });
                if create.inner.clicked() || enter {
                    outcome = NewOutcome::Create {
                        id: id.clone(),
                        name: self.name.trim().to_owned(),
                        base: self.base.clone(),
                    };
                }
                if kit::button(ui, Tone::Ghost, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                    outcome = NewOutcome::Cancel;
                }
            });
        });
        if modal.should_close() && matches!(outcome, NewOutcome::Open) {
            outcome = NewOutcome::Cancel;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use nordpil_backend::themes::{LoadedTheme, NORDIC_DARK};

    use super::*;

    #[test]
    fn new_ids_are_slugs_that_are_not_taken() {
        let taken = |id: &str| LoadedTheme {
            id: id.to_owned(),
            file: Ok(ThemeFile {
                name: id.to_owned(),
                base: NORDIC_DARK.to_owned(),
                ..ThemeFile::default()
            }),
            modified: None,
        };
        let themes = Themes::new(vec![taken("ocean"), taken("ocean-2")]);
        assert_eq!(new_id("  Deep  Sea!! ", &themes), "deep-sea");
        assert_eq!(new_id("Ocean", &themes), "ocean-3");
        assert_eq!(new_id("Blåbär", &themes), "blåbär");
        assert_eq!(new_id("Nordic dark", &themes), "nordic-dark-2");
        assert_eq!(new_id("!!!", &themes), "theme");
    }

    #[test]
    fn saved_files_hold_only_set_colors_in_token_order() {
        let mut draft = Draft::new("  ", NORDIC_DARK);
        draft.colors[index(Token::find("syntax_key").unwrap())] = Some(Rgba::rgb(0x112233));
        draft.colors[index(Token::find("main").unwrap())] = Some(Rgba([1, 2, 3, 4]));
        let file = draft.file("ocean");
        assert_eq!(file.name, "ocean");
        assert_eq!(
            file.colors,
            [
                ("main".to_owned(), "#01020304".to_owned()),
                ("syntax_key".to_owned(), "#112233".to_owned()),
            ]
        );
        assert_eq!(Draft::from_file(&file).colors, draft.colors);
    }
}
