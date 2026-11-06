//! The Ask AI dialog: a request, the agent's answer to check and edit, and
//! exactly what the agent was sent.

use eframe::egui::{self, RichText, ScrollArea};
use nordpil_backend::ai::Surface;
use nordpil_backend::settings::AiAgent;
use nordpil_query::{FieldHint, Mode};

use crate::ai::AskAi;
use crate::icons;
use crate::query_language::QueryLanguage;
use crate::theme;
use crate::widgets::kit::{self, Tone};

pub enum Outcome {
    /// Ask with the prompt, starting over.
    Start,
    /// Ask for a change to the answer.
    Refine,
    /// Stop the running request.
    Stop,
    /// Put the answer in the tab: the query (and run it), or the pipeline.
    Apply,
    /// From Documents: put a pipeline answer in the tab's Aggregate panel.
    OpenInAggregate,
    Close,
}

pub const AGENTS: [(AiAgent, Option<&str>, &str); 3] = [
    (AiAgent::Claude, None, "Claude"),
    (AiAgent::Codex, None, "Codex"),
    (AiAgent::Custom, None, "Custom"),
];

pub fn show(
    ctx: &egui::Context,
    state: &mut AskAi,
    collection: &str,
    fields: &[FieldHint],
    collections: &[String],
) -> Option<Outcome> {
    let mut outcome = None;
    let modal = kit::modal(ctx, egui::Id::unique(("ask_ai", state.tab_id))).show(ctx, |ui| {
        kit::dialog_width(ui, 680.0);
        let subtitle = match state.surface {
            Surface::Find => format!("A find query on {collection}"),
            Surface::Pipeline => format!("A pipeline on {collection}"),
        };
        kit::dialog_title(ui, "Ask AI", Some(&subtitle));
        let palette = theme::palette(ui);

        ui.horizontal(|ui| {
            ui.add_enabled_ui(!state.is_busy(), |ui| {
                kit::segmented(ui, &mut state.agent, &AGENTS, 30.0);
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Gets field names, types and indexes. Values in your query are hidden.",
                )
                .size(12.5)
                .color(palette.muted),
            );
        });
        ui.add_space(10.0);

        let hint = match state.surface {
            Surface::Find => "e.g. paid orders over 100 from last week, newest first",
            Surface::Pipeline => "e.g. revenue per customer this year, top 10",
        };
        let prompt = ui.add_enabled(
            !state.is_busy(),
            egui::TextEdit::multiline(&mut state.prompt)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        );
        prompt.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Request")
        });
        if state.focus {
            prompt.request_focus();
            state.focus = false;
        }
        let submit = prompt.has_focus()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if let Some(pending) = &state.pending {
                if kit::button(
                    ui,
                    Tone::Secondary,
                    Some(icons::SQUARE),
                    "Stop",
                    kit::TOOLBAR_HEIGHT,
                )
                .clicked()
                {
                    outcome = Some(Outcome::Stop);
                }
                ui.spinner();
                let seconds = pending.since.elapsed().as_secs();
                let doing = if pending.request.is_none() {
                    "Reading the indexes…".to_owned()
                } else if pending.retry {
                    format!("The answer didn't parse, asking again… {seconds} s")
                } else {
                    format!("Asking… {seconds} s")
                };
                ui.label(RichText::new(doing).color(palette.muted));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
            } else {
                let generate = kit::button(
                    ui,
                    Tone::Primary,
                    Some(icons::SPARKLES),
                    "Generate",
                    kit::TOOLBAR_HEIGHT,
                );
                if generate.clicked() || submit {
                    outcome = Some(Outcome::Start);
                }
            }
        });
        if let Some(error) = &state.error {
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT))
                        .color(palette.danger),
                )
                .wrap(),
            );
        }

        if let Some(preview) = &mut state.preview {
            ui.add_space(12.0);
            if preview.pipeline_kind {
                let language =
                    QueryLanguage::new(Mode::Pipeline, fields, &[]).collections(collections);
                state
                    .editors
                    .pipeline
                    .show(ui, &mut preview.pipeline, &language)
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Pipeline")
                    });
            } else {
                let label_width = 64.0;
                let editors = &mut state.editors;
                for (label, mode, editor, text) in [
                    (
                        "Filter",
                        Mode::Filter,
                        &mut editors.filter,
                        &mut preview.filter,
                    ),
                    (
                        "Project",
                        Mode::Fields,
                        &mut editors.project,
                        &mut preview.project,
                    ),
                    ("Sort", Mode::Fields, &mut editors.sort, &mut preview.sort),
                ] {
                    ui.horizontal_top(|ui| {
                        ui.add_sized(
                            [label_width, 30.0],
                            egui::Label::new(RichText::new(label).color(palette.muted)),
                        );
                        let language = QueryLanguage::new(mode, fields, &[]);
                        let width = ui.available_width();
                        ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
                            ui.set_width(width);
                            editor.show(ui, text, &language).response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::MultilineTextInput,
                                    true,
                                    label,
                                )
                            });
                        });
                    });
                }
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [label_width, 30.0],
                        egui::Label::new(RichText::new("Limit").color(palette.muted)),
                    );
                    ui.add(egui::TextEdit::singleline(&mut preview.limit).desired_width(120.0))
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Limit")
                        });
                });
            }
            if !state.explanation.is_empty() {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(RichText::new(&state.explanation).color(palette.muted)).wrap(),
                );
            }
            if state.surface == Surface::Find && preview.pipeline_kind {
                ui.label(
                    RichText::new("Find can't do this, so the answer is a pipeline.")
                        .size(12.5)
                        .color(palette.subtle),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let button = 90.0;
                let width = ui.available_width() - button - ui.spacing().item_spacing.x;
                let field = ui.add_enabled(
                    !state.is_busy(),
                    egui::TextEdit::singleline(&mut state.refine)
                        .desired_width(width)
                        .hint_text("Change it, e.g. only this year"),
                );
                let send = ui.add_enabled(
                    !state.is_busy(),
                    egui::Button::new("Refine").min_size(egui::vec2(button, 30.0)),
                );
                field.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Refine")
                });
                let entered =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if send.clicked() || entered {
                    outcome = Some(Outcome::Refine);
                }
            });
        }

        if let Some(sent) = &state.sent {
            ui.add_space(8.0);
            egui::CollapsingHeader::new("Sent to agent")
                .id_salt(("ask_ai_sent", state.tab_id))
                .show(ui, |ui| {
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(sent).monospace().size(12.0))
                                .selectable(true)
                                .wrap(),
                        );
                    });
                });
        }

        kit::dialog_footer(ui, |ui| {
            let ready = state.preview.is_some() && !state.is_busy();
            let pipeline = state
                .preview
                .as_ref()
                .is_some_and(|preview| preview.pipeline_kind);
            let (label, main) = match (state.surface, pipeline) {
                (Surface::Find, false) => ("Apply and run", Outcome::Apply),
                (Surface::Find, true) => ("Open in Aggregation", Outcome::OpenInAggregate),
                (Surface::Pipeline, _) => ("Replace stages", Outcome::Apply),
            };
            ui.add_enabled_ui(ready, |ui| {
                if kit::button(ui, Tone::Primary, None, label, kit::DIALOG_HEIGHT).clicked() {
                    outcome = Some(main);
                }
            });
            if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                outcome = Some(Outcome::Close);
            }
        });
    });
    if outcome.is_none() && modal.should_close() {
        outcome = Some(Outcome::Close);
    }
    outcome
}
