//! Settings: theme, text size, what Ctrl+K searches, id lookups and updates. Changes
//! apply at once and are saved to `config.toml`.

use eframe::egui::{self, CornerRadius, Frame, Margin, RichText, Stroke};
use nordpil_backend::settings::{
    AiAgent, AiSettings, GotoScope, Settings, TextSize, Theme, Updates,
};

use crate::theme;
use crate::widgets::kit::{self, Tone};

#[derive(Default)]
pub struct SettingsDialog {
    /// The AI text fields as typed; saved when one loses focus or the
    /// dialog closes, not on every key.
    ai: Option<AiDraft>,
}

struct AiDraft {
    claude_path: String,
    claude_model: String,
    codex_path: String,
    codex_model: String,
    program: String,
    /// One argument per line.
    args: String,
}

impl AiDraft {
    fn new(ai: &AiSettings) -> Self {
        Self {
            claude_path: ai.claude.path.clone(),
            claude_model: ai.claude.model.clone(),
            codex_path: ai.codex.path.clone(),
            codex_model: ai.codex.model.clone(),
            program: ai.custom.program.clone(),
            args: ai.custom.args.join("\n"),
        }
    }

    fn save(&self, ai: &mut AiSettings) {
        let trim = |text: &str| text.trim().to_owned();
        ai.claude.path = trim(&self.claude_path);
        ai.claude.model = trim(&self.claude_model);
        ai.codex.path = trim(&self.codex_path);
        ai.codex.model = trim(&self.codex_model);
        ai.custom.program = trim(&self.program);
        ai.custom.args = self
            .args
            .lines()
            .map(str::trim)
            .filter(|arg| !arg.is_empty())
            .map(str::to_owned)
            .collect();
    }
}

pub enum Outcome {
    Open,
    Close,
    Changed,
}

impl SettingsDialog {
    pub fn show(&mut self, ctx: &egui::Context, settings: &mut Settings) -> Outcome {
        let before = settings.clone();
        let mut outcome = Outcome::Open;
        let draft = self.ai.get_or_insert_with(|| AiDraft::new(&settings.ai));
        let mut save_draft = false;
        let modal = kit::modal(ctx, egui::Id::unique("settings_dialog")).show(ctx, |ui| {
            kit::dialog_width(ui, 560.0);
            kit::dialog_title(ui, "Settings", None);
            // Room for the title and footer; scrolls only in a short window.
            let height = (ui.ctx().content_rect().height() - 200.0).max(200.0);
            egui::ScrollArea::vertical()
                .max_height(height)
                .min_scrolled_height(height)
                .show(ui, |ui| {
                    let grid = egui::Grid::new("settings")
                        .num_columns(2)
                        .spacing([20.0, 18.0])
                        .min_col_width(160.0);
                    grid.show(ui, |ui| {
                        described(ui, "Theme", "System follows your OS");
                        kit::segmented(
                            ui,
                            &mut settings.theme,
                            &[
                                (Theme::System, None, "System"),
                                (Theme::Dark, None, "Dark"),
                                (Theme::Light, None, "Light"),
                            ],
                            38.0,
                        );
                        ui.end_row();

                        described(ui, "Text size", "Body text 12.5, 14 or 15 px");
                        kit::segmented(
                            ui,
                            &mut settings.text_size,
                            &[
                                (TextSize::Compact, None, "Compact"),
                                (TextSize::Comfortable, None, "Comfortable"),
                                (TextSize::Large, None, "Large"),
                            ],
                            38.0,
                        );
                        ui.end_row();

                        ui.vertical(|ui| {
                            ui.label("Go to collection");
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("What").size(12.5).weak());
                                kit::shortcut(ui, &crate::shortcuts::GO_TO);
                                ui.label(RichText::new("searches").size(12.5).weak());
                            });
                        });
                        ui.vertical(|ui| {
                            ui.set_width(330.0);
                            choice(
                                ui,
                                &mut settings.goto_scope,
                                GotoScope::AllSaved,
                                "All saved connections",
                                "Remembers collection names. Picking one on a closed \
                         connection connects to it first.",
                            );
                            ui.add_space(4.0);
                            choice(
                                ui,
                                &mut settings.goto_scope,
                                GotoScope::OpenOnly,
                                "Open connections only",
                                "Lists collections from connections that are open now.",
                            );
                        });
                        ui.end_row();

                        described(ui, "Look up references", "Off by default");
                        ui.vertical(|ui| {
                            ui.set_width(330.0);
                            ui.checkbox(
                                &mut settings.look_up_references,
                                "Open referenced documents",
                            );
                            ui.horizontal(|ui| {
                                ui.add_space(26.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Right-click a field like userId to open the matching \
                                     document in users, in a new tab.",
                                        )
                                        .size(13.0)
                                        .color(theme::palette(ui).muted),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                        ui.end_row();

                        described(ui, "Preview sample", "Aggregation stage previews");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut settings.aggregate.sample_size)
                                    .range(100..=10_000_000)
                                    .speed(1000.0),
                            )
                            .on_hover_text(
                                "While sampling is on, previews read at most this many documents.",
                            );
                            ui.label(RichText::new("documents").size(13.0).weak());
                        });
                        ui.end_row();

                        described(ui, "Ask AI", "Writes queries and pipelines");
                        ui.vertical(|ui| {
                            ui.set_width(330.0);
                            save_draft |= ai_settings(ui, &mut settings.ai, draft);
                        });
                        ui.end_row();

                        described(ui, "Updates", "From GitHub releases");
                        kit::segmented(
                            ui,
                            &mut settings.updates,
                            &[
                                (Updates::Automatic, None, "Install"),
                                (Updates::Notify, None, "Notify"),
                                (Updates::Off, None, "Off"),
                            ],
                            38.0,
                        );
                        ui.end_row();
                    });
                });
            kit::dialog_footer(ui, |ui| {
                if kit::button(ui, Tone::Primary, None, "Done", kit::DIALOG_HEIGHT).clicked() {
                    outcome = Outcome::Close;
                }
            });
        });
        if matches!(outcome, Outcome::Close) || modal.should_close() {
            draft.save(&mut settings.ai);
            return Outcome::Close;
        }
        if save_draft {
            draft.save(&mut settings.ai);
        }
        if *settings != before {
            outcome = Outcome::Changed;
        }
        outcome
    }
}

/// The agent and its options. Returns true when a text field lost focus.
fn ai_settings(ui: &mut egui::Ui, ai: &mut AiSettings, draft: &mut AiDraft) -> bool {
    let palette = theme::palette(ui);
    let mut lost_focus = false;
    kit::segmented(
        ui,
        &mut ai.agent,
        &[
            (AiAgent::Off, None, "Off"),
            (AiAgent::Claude, None, "Claude"),
            (AiAgent::Codex, None, "Codex"),
            (AiAgent::Custom, None, "Custom"),
        ],
        34.0,
    );
    let mut field = |ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str| {
        ui.label(RichText::new(label).size(12.5).color(palette.muted));
        let response = ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        );
        let name = label.to_owned();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &name));
        lost_focus |= response.lost_focus();
    };
    match ai.agent {
        AiAgent::Off => {}
        AiAgent::Claude => {
            ui.add_space(6.0);
            field(
                ui,
                "Claude program",
                &mut draft.claude_path,
                "claude, found on your PATH",
            );
            field(
                ui,
                "Claude model",
                &mut draft.claude_model,
                "haiku, the fast tier",
            );
        }
        AiAgent::Codex => {
            ui.add_space(6.0);
            field(
                ui,
                "Codex program",
                &mut draft.codex_path,
                "codex, found on your PATH",
            );
            field(
                ui,
                "Codex model",
                &mut draft.codex_model,
                "gpt-6-luna, the fast tier",
            );
        }
        AiAgent::Custom => {
            ui.add_space(6.0);
            field(ui, "Program", &mut draft.program, "/path/to/agent");
            ui.label(
                RichText::new("Arguments, one per line")
                    .size(12.5)
                    .color(palette.muted),
            );
            let response = ui.add(
                egui::TextEdit::multiline(&mut draft.args)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("{schema_file} becomes the answer's JSON Schema file"),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Arguments")
            });
            lost_focus |= response.lost_focus();
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "It reads the prompt on stdin and prints a JSON object; \
                         the last one in its output is used.",
                    )
                    .size(12.5)
                    .color(palette.muted),
                )
                .wrap(),
            );
        }
    }
    if ai.agent != AiAgent::Off {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Stop after").size(12.5).color(palette.muted));
            ui.add(
                egui::DragValue::new(&mut ai.timeout_secs)
                    .range(10..=900)
                    .suffix(" s"),
            );
        });
        ui.add(
            egui::Label::new(
                RichText::new(
                    "The agent gets field names, types and indexes, never values. \
                     Values in your query are hidden.",
                )
                .size(12.5)
                .color(palette.muted),
            )
            .wrap(),
        );
    }
    lost_focus
}

fn described(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(title);
        ui.label(RichText::new(description).size(12.5).weak());
    });
}

/// A radio option drawn as a card, highlighted when chosen.
fn choice(ui: &mut egui::Ui, value: &mut GotoScope, option: GotoScope, title: &str, text: &str) {
    let palette = theme::palette(ui);
    let selected = *value == option;
    let response = Frame::new()
        .fill(if selected {
            palette.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(Stroke::new(
            1.0,
            if selected {
                palette.accent
            } else {
                palette.line
            },
        ))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let radio = ui.radio(selected, title);
                if radio.clicked() {
                    *value = option;
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(26.0);
                ui.add(
                    egui::Label::new(RichText::new(text).size(13.0).color(palette.muted)).wrap(),
                );
            });
        })
        .response
        .interact(egui::Sense::click());
    if response.clicked() {
        *value = option;
    }
}
