//! Settings: theme, text size, what Ctrl+K searches and id lookups. Changes
//! apply at once and are saved to `config.toml`.

use eframe::egui::{self, CornerRadius, Frame, Margin, RichText, Stroke};
use nordpil_backend::settings::{GotoScope, Settings, TextSize, Theme};

use crate::theme;
use crate::widgets::kit::{self, Tone};

pub struct SettingsDialog;

pub enum Outcome {
    Open,
    Close,
    Changed,
}

impl SettingsDialog {
    pub fn show(&mut self, ctx: &egui::Context, settings: &mut Settings) -> Outcome {
        let before = *settings;
        let mut outcome = Outcome::Open;
        let modal = kit::modal(ctx, egui::Id::unique("settings_dialog")).show(ctx, |ui| {
            kit::dialog_width(ui, 560.0);
            kit::dialog_title(ui, "Settings", None);
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
                        kit::keys(ui, &["Ctrl", "K"]);
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
            });
            kit::dialog_footer(ui, |ui| {
                if kit::button(ui, Tone::Primary, None, "Done", kit::DIALOG_HEIGHT).clicked() {
                    outcome = Outcome::Close;
                }
            });
        });
        if *settings != before {
            outcome = Outcome::Changed;
        } else if modal.should_close() {
            outcome = Outcome::Close;
        }
        outcome
    }
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
