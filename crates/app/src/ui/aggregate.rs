//! The aggregation panel: the stage builder on the left, each stage with a
//! preview of its output, and the whole pipeline's results on the right.

use bson::Document;
use eframe::egui::{
    self, Align, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextEdit,
};
use nordpil_backend::PipelineText;
use nordpil_backend::settings::AggregateSettings;
use nordpil_query::Mode;
use nordpil_query::pipeline::STAGES;
use uuid::Uuid;

use crate::aggregate::{AggregateState, Paste, Preview, StageCard};
use crate::icons;
use crate::lookup::References;
use crate::query_language::QueryLanguage;
use crate::shortcuts;
use crate::tabs::{CollectionTab, PAGE_SIZES};
use crate::theme;
use crate::ui::documents::{self, DocumentPage, FieldMenus, TabAction, VIEWS, page_button};
use crate::widgets::code_editor;
use crate::widgets::kit::{self, Tone};
use crate::widgets::split::{Pane, Split};

/// What the panel needs the app for: the backend, other tabs, settings.
pub enum AggregateAction {
    Run,
    Stop,
    Refresh,
    /// Show this page of the results.
    Page(u64),
    Remove(usize),
    /// Fill in a pipeline from the history.
    Apply(PipelineText),
    SetFavorite {
        id: Uuid,
        favorite: bool,
    },
    Delete(Uuid),
    /// The answer to "Run this pipeline that writes?".
    Confirm(bool),
    /// Open the collection a pipeline wrote to.
    OpenWritten,
    Paste(String),
    OpenInShell,
    Explain,
    Export,
    /// The split was dragged or reset.
    SaveLayout,
    Lookup {
        collection: String,
        filter: Document,
    },
    /// Open Ask AI for the pipeline.
    AskAi,
}

/// What the panel uses besides its tab.
pub struct Surroundings<'a> {
    /// The database's collections, for completion.
    pub collections: &'a [String],
    pub settings: &'a mut AggregateSettings,
    /// Where id fields of results can be looked up; `None` when lookups are off.
    pub references: Option<&'a References>,
    pub read_only: bool,
    /// Shows Ask AI.
    pub ai: bool,
}

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    surroundings: Surroundings,
) -> Option<AggregateAction> {
    let Surroundings {
        collections,
        settings,
        references,
        read_only,
        ai,
    } = surroundings;
    let palette = theme::palette(ui);
    let mut action = toolbar(ui, tab, settings.sample_size, ai);
    if tab.aggregate.show_options {
        ui.add_space(8.0);
        action = options(ui, tab).or(action);
    }
    for error in &tab.aggregate.errors {
        ui.label(RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT)).color(palette.danger));
    }
    ui.add_space(10.0);
    let split = Split::new(egui::Id::unique(("aggregate_split", tab.id)), true)
        .default_share(AggregateSettings::SPLIT);
    let mut stages_action = None;
    let mut results_action = None;
    let done = split.show(ui, &mut settings.split, |ui, pane| match pane {
        Pane::First => stages_action = stages(ui, tab, collections),
        Pane::Second => results_action = results(ui, tab, references),
    });
    if done {
        action = action.or(Some(AggregateAction::SaveLayout));
    }
    let dialog_action = dialogs(ui.ctx(), tab, read_only);
    action
        .or(stages_action)
        .or(results_action)
        .or(dialog_action)
}

fn toolbar(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    sample_size: u64,
    ai: bool,
) -> Option<AggregateAction> {
    let mut action = None;
    let height = kit::TOOLBAR_HEIGHT;
    let state = &mut tab.aggregate;
    ui.horizontal(|ui| {
        if state.running() {
            let stop = kit::button(ui, Tone::Secondary, Some(icons::SQUARE), "Stop", height)
                .on_hover_text(format!(
                    "Stop the pipeline, on the server too ({})",
                    shortcuts::text(ui.ctx(), &shortcuts::STOP_SCRIPT)
                ));
            if stop.clicked() {
                action = Some(AggregateAction::Stop);
            }
            ui.spinner();
        } else {
            let run = kit::button(ui, Tone::Primary, Some(icons::PLAY), "Run", height)
                .on_hover_text(format!(
                    "Run the pipeline ({})",
                    shortcuts::text(ui.ctx(), &shortcuts::RUN_STATEMENT)
                ));
            if run.clicked() {
                action = Some(AggregateAction::Run);
            }
        }
        let add = kit::button(ui, Tone::Ghost, Some(icons::PLUS), "Stage", height)
            .on_hover_text("Add a stage at the end");
        let at = state.stages.len();
        egui::Popup::menu(&add).show(|ui| {
            if let Some(name) = stage_list(ui, egui::Id::unique(("add_stage", tab.id))) {
                state.insert(at, name);
                ui.close();
            }
        });
        let mut sample = state.sample;
        let checkbox = ui
            .checkbox(&mut sample, "Sample previews")
            .on_hover_text(format!(
                "Previews read at most {} documents. Run always reads them all.",
                documents::thousands(sample_size)
            ));
        if checkbox.changed() {
            state.set_sample(sample);
        }
        let options = if state.show_options {
            "Hide options"
        } else {
            "Options"
        };
        if kit::button(ui, Tone::Ghost, Some(icons::SLIDERS), options, height).clicked() {
            state.show_options = !state.show_options;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let more = dots_button(ui, "More", height);
            egui::Popup::menu(&more).show(|ui| {
                if ui.button("Copy pipeline").clicked() {
                    ui.ctx().copy_text(state.pipeline_text());
                }
                if ui.button("Copy as shell code").clicked() {
                    ui.ctx()
                        .copy_text(state.shell_statement(&tab.target.collection));
                }
                if ai && ui.button("Ask AI…").clicked() {
                    action = Some(AggregateAction::AskAi);
                }
                if ui.button("Paste pipeline…").clicked() {
                    state.paste = Some(Paste {
                        editor: code_editor::CodeEditor::new(
                            egui::Id::unique(("paste_pipeline", tab.id)),
                            true,
                        )
                        .rows(12)
                        .hint("[{ $match: {} }, { $limit: 10 }]"),
                        text: String::new(),
                        error: None,
                    });
                }
                ui.separator();
                if ui.button("Open in Shell").clicked() {
                    action = Some(AggregateAction::OpenInShell);
                }
                if ui.button("Explain").clicked() {
                    action = Some(AggregateAction::Explain);
                }
            });
            if ai {
                let ask = kit::button(ui, Tone::Ghost, Some(icons::SPARKLES), "Ask AI", height)
                    .on_hover_text(format!(
                        "Describe the pipeline and let an agent write it ({})",
                        shortcuts::text(ui.ctx(), &shortcuts::ASK_AI)
                    ));
                if ask.clicked() {
                    action = Some(AggregateAction::AskAi);
                }
            }
            let history = kit::button(ui, Tone::Ghost, Some(icons::HISTORY), "History", height);
            egui::Popup::menu(&history).show(|ui| {
                if let Some(chosen) = history_menu(ui, state) {
                    action = Some(chosen);
                    ui.close();
                }
            });
        });
    });
    action
}

fn options(ui: &mut egui::Ui, tab: &mut CollectionTab) -> Option<AggregateAction> {
    let mut action = None;
    let palette = theme::palette(ui);
    let state = &mut tab.aggregate;
    let before = (
        state.collation.clone(),
        state.hint.clone(),
        state.max_time_ms.clone(),
        state.allow_disk_use,
    );
    let fields = &tab.fields;
    egui::Grid::new(("aggregate_options", tab.id))
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            let mut field = |ui: &mut egui::Ui,
                             name: &str,
                             editor: &mut code_editor::CodeEditor,
                             text: &mut String,
                             mode: Mode| {
                let label = ui.label(RichText::new(name).color(palette.muted));
                let language = QueryLanguage::new(mode, fields, &[]);
                let output = editor.show(ui, text, &language);
                output.response.labelled_by(label.id);
                if output.submitted {
                    action = Some(AggregateAction::Run);
                }
                ui.end_row();
            };
            field(
                ui,
                "Collation",
                &mut state.collation_editor,
                &mut state.collation,
                Mode::Collation,
            );
            field(
                ui,
                "Hint",
                &mut state.hint_editor,
                &mut state.hint,
                Mode::Fields,
            );
            let label = ui.label(RichText::new("Max time (ms)").color(palette.muted));
            ui.add(
                TextEdit::singleline(&mut state.max_time_ms)
                    .hint_text("60000")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut state.allow_disk_use, "Allow disk use");
            ui.end_row();
        });
    let after = (
        state.collation.clone(),
        state.hint.clone(),
        state.max_time_ms.clone(),
        state.allow_disk_use,
    );
    if before != after {
        state.options_edited();
    }
    action
}

/// The stages, each in a card, with a way to add one between any two.
fn stages(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    collections: &[String],
) -> Option<AggregateAction> {
    let palette = theme::palette(ui);
    let mut action = None;
    ScrollArea::vertical()
        .id_salt(("aggregate_stages", tab.id))
        .auto_shrink(false)
        .show(ui, |ui| {
            if tab.aggregate.stages.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Add stages to build a pipeline. Each stage shows a preview of its output.",
                    )
                    .color(palette.muted),
                );
                ui.add_space(8.0);
            }
            for index in 0..tab.aggregate.stages.len() {
                if index > 0 {
                    insert_between(ui, tab, index);
                }
                if let Some(chosen) = stage_card(ui, tab, index, collections) {
                    action = Some(chosen);
                }
            }
            ui.add_space(8.0);
            let add = kit::button(ui, Tone::Secondary, Some(icons::PLUS), "Add stage", 30.0);
            let at = tab.aggregate.stages.len();
            egui::Popup::menu(&add).show(|ui| {
                if let Some(name) = stage_list(ui, egui::Id::unique(("add_stage_end", tab.id))) {
                    tab.aggregate.insert(at, name);
                    ui.close();
                }
            });
            ui.add_space(12.0);
        });
    action
}

/// A small `+` between two cards that inserts a stage there.
fn insert_between(ui: &mut egui::Ui, tab: &mut CollectionTab, index: usize) {
    ui.vertical_centered(|ui| {
        let label = format!("Insert a stage before stage {}", index + 1);
        let plus = kit::icon_button(ui, icons::PLUS, &label, 20.0).on_hover_text(&label);
        egui::Popup::menu(&plus).show(|ui| {
            let id = egui::Id::unique(("insert_stage", tab.id, index));
            if let Some(name) = stage_list(ui, id) {
                tab.aggregate.insert(index, name);
                ui.close();
            }
        });
    });
}

fn stage_card(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    index: usize,
    collections: &[String],
) -> Option<AggregateAction> {
    let palette = theme::palette(ui);
    let mut action = None;
    let (fields, values) = tab.aggregate.fields_for(index, &tab.fields);
    let state = &mut tab.aggregate;
    let card_id = state.stages[index].id;
    let focus = state.focus.take_if(|(id, _)| *id == card_id);
    let enabled = state.stages[index].enabled;
    let mut rename = None;
    let mut toggle = None;
    let mut edited = false;
    let mut duplicate = false;
    let mut insert = None;
    let frame = kit::card(ui)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let card = &mut state.stages[index];
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.dnd_drag_source(egui::Id::unique(("stage_drag", card.id)), index, |ui| {
                    grip(ui);
                })
                .response
                .on_hover_text("Drag to move the stage");
                ui.label(RichText::new(format!("{}", index + 1)).color(palette.subtle));
                let picker = ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{}  {}", card.name, icons::CHEVRON_DOWN))
                                .monospace()
                                .strong(),
                        )
                        .frame_when_inactive(false),
                    )
                    .on_hover_text(
                        nordpil_query::pipeline::stage(&card.name).map_or("", |s| s.detail),
                    );
                egui::Popup::menu(&picker).show(|ui| {
                    let id = egui::Id::unique(("rename_stage", card.id));
                    if let Some(name) = stage_list(ui, id) {
                        rename = Some(name);
                        ui.close();
                    }
                });
                let (icon, label) = if card.enabled {
                    (icons::EYE, "Turn the stage off")
                } else {
                    (icons::EYE_OFF, "Turn the stage on")
                };
                if kit::icon_button(ui, icon, label, 26.0)
                    .on_hover_text(label)
                    .clicked()
                {
                    toggle = Some(!card.enabled);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let more = dots_button(ui, "Stage menu", 26.0);
                    egui::Popup::menu(&more).show(|ui| {
                        if ui.button("Duplicate").clicked() {
                            duplicate = true;
                        }
                        ui.menu_button("Insert above", |ui| {
                            let id = egui::Id::unique(("insert_above", card.id));
                            if let Some(name) = stage_list(ui, id) {
                                insert = Some((index, name));
                                ui.close();
                            }
                        });
                        ui.menu_button("Insert below", |ui| {
                            let id = egui::Id::unique(("insert_below", card.id));
                            if let Some(name) = stage_list(ui, id) {
                                insert = Some((index + 1, name));
                                ui.close();
                            }
                        });
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            action = Some(AggregateAction::Remove(index));
                        }
                    });
                    let (chevron, label) = if card.collapsed {
                        (icons::CHEVRON_RIGHT, "Expand the stage")
                    } else {
                        (icons::CHEVRON_DOWN, "Collapse the stage")
                    };
                    if kit::icon_button(ui, chevron, label, 26.0).clicked() {
                        card.collapsed = !card.collapsed;
                        edited = true;
                    }
                    preview_status(ui, card);
                });
            });
            if card.collapsed {
                return;
            }
            ui.add_space(4.0);
            if !card.enabled {
                ui.multiply_opacity(0.5);
            }
            let mode =
                nordpil_query::pipeline::stage(&card.name).map_or(Mode::Document, Mode::Stage);
            let language = QueryLanguage::new(mode, &fields, &values).collections(collections);
            if let Some((_, cursor)) = focus {
                card.editor.focus_at(ui.ctx(), cursor);
            }
            let before = card.body.clone();
            let output = card.editor.show(ui, &mut card.body, &language);
            let name = format!("Stage {} {}", index + 1, card.name);
            output.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, &name)
            });
            if output.submitted {
                action = Some(AggregateAction::Run);
            }
            if card.body != before {
                edited = true;
                card.check();
            }
            if let Some(error) = &card.error {
                ui.label(
                    RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT))
                        .size(12.5)
                        .color(palette.danger),
                );
            }
            preview(ui, card, enabled);
        });
    let response = frame.response;
    if let Some(from) = response.dnd_release_payload::<usize>() {
        state.move_stage(*from, index);
    } else if response.dnd_hover_payload::<usize>().is_some() {
        ui.painter().rect_stroke(
            response.rect,
            CornerRadius::same(12),
            Stroke::new(2.0, palette.accent),
            StrokeKind::Inside,
        );
    }
    if let Some(name) = rename {
        state.rename(index, name);
    }
    if let Some(enabled) = toggle {
        state.set_enabled(index, enabled);
    }
    if duplicate {
        state.duplicate(index);
    }
    if let Some((at, name)) = insert {
        state.insert(at, name);
    }
    if edited {
        state.body_edited(index);
    }
    action
}

/// The preview's summary in the card header: a spinner, a count, or a word
/// on why there is none.
fn preview_status(ui: &mut egui::Ui, card: &StageCard) {
    let palette = theme::palette(ui);
    if card.loading() {
        ui.spinner();
        return;
    }
    let text = match &card.preview {
        Preview::Documents { documents, .. } if documents.is_empty() => "no documents".to_owned(),
        Preview::Documents { documents, .. } => documents::plural(documents.len(), "document"),
        Preview::TimedOut => "timed out".to_owned(),
        Preview::Failed(_) => "failed".to_owned(),
        Preview::Skipped(_) | Preview::None => return,
    };
    ui.label(RichText::new(text).size(12.5).color(palette.subtle));
}

/// The first documents the stage gives, side by side, or why there are none.
fn preview(ui: &mut egui::Ui, card: &StageCard, enabled: bool) {
    let palette = theme::palette(ui);
    let note = |ui: &mut egui::Ui, text: &str, color| {
        ui.add_space(4.0);
        ui.label(RichText::new(text).size(12.5).color(color));
    };
    match &card.preview {
        _ if !enabled => note(ui, "Off: the pipeline skips this stage.", palette.subtle),
        Preview::None => {}
        Preview::Skipped(reason) => note(ui, reason, palette.subtle),
        Preview::TimedOut => note(
            ui,
            "The preview took over 5 s. Turn on sampling, or run the pipeline.",
            palette.muted,
        ),
        Preview::Failed(message) => note(
            ui,
            &format!("{}  {message}", icons::CIRCLE_ALERT),
            palette.danger,
        ),
        Preview::Documents { texts, sampled, .. } => {
            ui.add_space(6.0);
            if texts.is_empty() {
                ui.label(
                    RichText::new("No documents come out of this stage.")
                        .size(12.5)
                        .color(palette.muted),
                );
                return;
            }
            if *sampled {
                kit::pill(ui, "sampled", false)
                    .on_hover_text("From a sample of the collection; Run reads all of it.");
                ui.add_space(4.0);
            }
            ScrollArea::horizontal()
                .id_salt(("stage_preview", card.id))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for (text, spans) in texts {
                            preview_card(ui, text, spans);
                        }
                    });
                });
        }
    }
}

fn preview_card(ui: &mut egui::Ui, text: &str, spans: &[code_editor::Span]) {
    let palette = theme::palette(ui);
    Frame::new()
        .fill(palette.raised)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(210.0);
            let mut job = code_editor::layout_job(ui, text, spans);
            job.wrap.max_width = 210.0;
            job.wrap.max_rows = 16;
            ui.add(egui::Label::new(job).selectable(true));
        });
}

/// Stage names to pick from, narrowed by a search box. Returns the pick.
fn stage_list(ui: &mut egui::Ui, id: egui::Id) -> Option<&'static str> {
    let mut search: String = ui.data(|data| data.get_temp(id)).unwrap_or_default();
    let edit = ui.add(
        TextEdit::singleline(&mut search)
            .hint_text("Search stages")
            .desired_width(260.0),
    );
    edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Search stages"));
    if search.is_empty() && !edit.has_focus() && ui.memory(|memory| memory.focused().is_none()) {
        edit.request_focus();
    }
    let needle = search.trim().trim_start_matches('$').to_lowercase();
    let matching: Vec<_> = STAGES
        .iter()
        .filter(|stage| {
            needle.is_empty()
                || stage.name[1..].to_lowercase().contains(&needle)
                || stage.detail.to_lowercase().contains(&needle)
        })
        .collect();
    let mut chosen = None;
    if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        chosen = matching.first().map(|stage| stage.name);
    }
    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        for stage in &matching {
            ui.horizontal(|ui| {
                let button = ui.add(
                    egui::Button::new(RichText::new(stage.name).monospace())
                        .frame_when_inactive(false)
                        .min_size(egui::vec2(140.0, 0.0)),
                );
                if button.clicked() {
                    chosen = Some(stage.name);
                }
                ui.weak(stage.detail);
            });
        }
        if matching.is_empty() {
            ui.weak("No stage matches.");
        }
    });
    ui.data_mut(|data| {
        if chosen.is_some() {
            data.remove::<String>(id);
        } else {
            data.insert_temp(id, search);
        }
    });
    chosen
}

fn history_menu(ui: &mut egui::Ui, state: &AggregateState) -> Option<AggregateAction> {
    if state.history.is_empty() {
        ui.weak("Pipelines you run show up here.");
        return None;
    }
    ui.set_max_width(480.0);
    let mut action = None;
    let mut heading = None;
    for saved in &state.history {
        let section = if saved.favorite {
            "Favorites"
        } else {
            "Recent"
        };
        if heading != Some(section) {
            if heading.is_some() {
                ui.separator();
            }
            kit::section_label(ui, section);
            heading = Some(section);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let palette = theme::palette(ui);
            let (label, color) = if saved.favorite {
                ("Remove from favorites", palette.warn)
            } else {
                ("Add to favorites", palette.subtle)
            };
            let star = ui
                .add(
                    egui::Button::new(RichText::new(icons::STAR).size(14.0).color(color))
                        .frame_when_inactive(false)
                        .min_size(egui::vec2(26.0, 26.0)),
                )
                .on_hover_text(label);
            star.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            if star.clicked() {
                action = Some(AggregateAction::SetFavorite {
                    id: saved.id,
                    favorite: !saved.favorite,
                });
            }
            if kit::icon_button(ui, icons::TRASH, "Delete pipeline", 26.0).clicked() {
                action = Some(AggregateAction::Delete(saved.id));
            }
            ui.add_space(4.0);
            let summary = summary(&saved.pipeline);
            let response = ui.add(
                egui::Button::new(RichText::new(&summary).monospace())
                    .frame(false)
                    .truncate(),
            );
            if response.on_hover_text(full_text(&saved.pipeline)).clicked() {
                action = Some(AggregateAction::Apply(saved.pipeline.clone()));
            }
        });
    }
    action
}

/// E.g. "$match → $group → $sort".
fn summary(pipeline: &PipelineText) -> String {
    if pipeline.stages.is_empty() {
        return "(no stages)".to_owned();
    }
    pipeline
        .stages
        .iter()
        .map(|stage| stage.name.as_str())
        .collect::<Vec<_>>()
        .join(" → ")
}

fn full_text(pipeline: &PipelineText) -> String {
    nordpil_query::pipeline::join_pipeline(
        pipeline
            .stages
            .iter()
            .filter(|stage| stage.enabled)
            .map(|stage| (stage.name.as_str(), stage.body.as_str())),
    )
}

/// The run's results: what it wrote, or its pages of documents.
fn results(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    references: Option<&References>,
) -> Option<AggregateAction> {
    let palette = theme::palette(ui);
    let mut action = None;
    let page_size = tab.page_size;
    let state = &mut tab.aggregate;
    if let Some(written) = &state.written {
        let (database, collection) = &written.target;
        match &written.result {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Writing to {database}.{collection}…"));
                });
            }
            Some(Ok(())) => {
                ui.label(format!("Wrote to {database}.{collection}."));
                ui.add_space(6.0);
                if kit::button(
                    ui,
                    Tone::Secondary,
                    None,
                    &format!("Open {collection}"),
                    kit::TOOLBAR_HEIGHT,
                )
                .clicked()
                {
                    action = Some(AggregateAction::OpenWritten);
                }
            }
            Some(Err(message)) => {
                ui.label(
                    RichText::new(format!("{}  {message}", icons::CIRCLE_ALERT))
                        .color(palette.danger),
                );
            }
        }
        return action;
    }
    let Some(run) = &state.run else {
        ui.label(
            RichText::new("Run the pipeline to see all of its results here.").color(palette.muted),
        );
        return None;
    };
    // The view switch, export, then paging on the right.
    ui.horizontal(|ui| {
        let labels = ui.available_width() >= kit::segmented_width(ui, &VIEWS, true) + 360.0;
        kit::segmented_with_labels(
            ui,
            &mut state.view,
            &VIEWS,
            kit::TOOLBAR_HEIGHT + 2.0,
            labels,
        );
        kit::divider(ui, 22.0);
        if kit::icon_button(ui, icons::DOWNLOAD, "Export", kit::TOOLBAR_HEIGHT)
            .on_hover_text("Export the pipeline's results")
            .clicked()
        {
            action = Some(AggregateAction::Export);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if kit::icon_button(ui, icons::REFRESH, "Refresh", kit::TOOLBAR_HEIGHT).clicked() {
                action = Some(AggregateAction::Refresh);
            }
            let before = tab.page_size;
            let combo = egui::ComboBox::from_id_salt(("aggregate_page_size", tab.id))
                .width(66.0)
                .selected_text(tab.page_size.to_string())
                .show_ui(ui, |ui| {
                    for size in PAGE_SIZES {
                        ui.selectable_value(&mut tab.page_size, size, size.to_string());
                    }
                });
            let label = ui.label(RichText::new("Per page").size(13.0).color(palette.subtle));
            combo.response.labelled_by(label.id);
            if tab.page_size != before {
                // Stay at the first document shown.
                action = Some(AggregateAction::Page(run.page * before / tab.page_size));
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(run.has_next(page_size) && !run.loading(), |ui| {
                if page_button(ui, icons::CHEVRON_RIGHT, "Next page").clicked() {
                    action = Some(AggregateAction::Page(run.page + 1));
                }
            });
            ui.add_enabled_ui(run.page > 0, |ui| {
                if page_button(ui, icons::CHEVRON_LEFT, "Previous page").clicked() {
                    action = Some(AggregateAction::Page(run.page - 1));
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(range_text(run, page_size))
                    .size(13.0)
                    .color(palette.muted),
            );
            if run.loading() {
                ui.spinner();
            }
        });
    });
    ui.add_space(10.0);
    if let Some(error) = &run.error {
        ui.label(RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT)).color(palette.danger));
        return action;
    }
    let Some(current) = &run.current else {
        if run.stopped {
            ui.label(RichText::new("Stopped.").color(palette.muted));
        } else if run.loading() {
            ui.spinner();
        }
        return action;
    };
    if current.documents.is_empty() {
        ui.label(RichText::new("No documents.").color(palette.muted));
        return action;
    }
    let page = DocumentPage {
        results: current,
        scope: egui::Id::unique(("aggregate_results", tab.id)),
        page: run.page,
        field_menus: FieldMenus::Pipeline,
    };
    let mut view_action = None;
    documents::page_view(ui, state.view, &page, true, references, &mut view_action);
    match view_action {
        Some(TabAction::AddToFilter { path, value }) => state.add_to_match(&path, &value),
        Some(TabAction::Lookup { collection, filter }) => {
            action = Some(AggregateAction::Lookup { collection, filter });
        }
        _ => {}
    }
    action
}

/// E.g. "21–40 of 1,234".
fn range_text(run: &crate::aggregate::Run, page_size: u64) -> String {
    let shown = run.current.as_ref().map_or(0, |r| r.documents.len() as u64);
    let total = match run.count {
        Some(Some(total)) => documents::thousands(total),
        Some(None) => "many".to_owned(),
        None if run.exhausted => documents::thousands(run.documents.len() as u64),
        None => "…".to_owned(),
    };
    if shown == 0 {
        return format!("0 of {total}");
    }
    let first = run.page * page_size + 1;
    format!(
        "{}–{} of {total}",
        documents::thousands(first),
        documents::thousands(first + shown - 1)
    )
}

/// The write confirmation, Explain and Paste pipeline dialogs.
fn dialogs(
    ctx: &egui::Context,
    tab: &mut CollectionTab,
    read_only: bool,
) -> Option<AggregateAction> {
    let mut action = None;
    let state = &mut tab.aggregate;
    if let Some(confirm) = &state.confirm {
        let (database, collection) = &confirm.target;
        let mut answer = None;
        let modal =
            kit::modal(ctx, egui::Id::unique(("aggregate_write", tab.id))).show(ctx, |ui| {
                kit::dialog_width(ui, 440.0);
                let subtitle = format!("Aggregation in {}", tab.target.collection);
                kit::dialog_title(ui, "Run a pipeline that writes?", Some(&subtitle));
                if read_only {
                    ui.label("This connection is read-only, so the pipeline can't write.");
                } else if confirm.merges {
                    ui.label(format!(
                        "$merge merges the results into {database}.{collection}."
                    ));
                } else {
                    ui.label(format!(
                        "$out replaces {database}.{collection} with the results."
                    ));
                }
                kit::dialog_footer(ui, |ui| {
                    if !read_only
                        && kit::button(ui, Tone::Danger, None, "Run and write", kit::DIALOG_HEIGHT)
                            .clicked()
                    {
                        answer = Some(true);
                    }
                    if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT)
                        .clicked()
                    {
                        answer = Some(false);
                    }
                });
            });
        if modal.should_close() {
            answer = answer.or(Some(false));
        }
        if let Some(answer) = answer {
            action = Some(AggregateAction::Confirm(answer));
        }
    }
    if let Some(explain) = &state.explain {
        let mut close = false;
        let modal =
            kit::modal(ctx, egui::Id::unique(("aggregate_explain", tab.id))).show(ctx, |ui| {
                kit::dialog_width(ui, 640.0);
                kit::dialog_title(ui, "Explain", Some("The server's plan for the pipeline"));
                match &explain.result {
                    None => {
                        ui.spinner();
                    }
                    Some(Err(message)) => {
                        ui.label(
                            RichText::new(format!("{}  {message}", icons::CIRCLE_ALERT))
                                .color(theme::palette(ui).danger),
                        );
                    }
                    Some(Ok(plan)) => {
                        let spans = nordpil_query::highlight(plan);
                        ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                            let mut job = code_editor::layout_job(ui, plan, &spans);
                            job.wrap.max_width = ui.available_width();
                            ui.add(egui::Label::new(job).selectable(true));
                        });
                    }
                }
                kit::dialog_footer(ui, |ui| {
                    if let Some(Ok(plan)) = &explain.result
                        && kit::button(
                            ui,
                            Tone::Secondary,
                            Some(icons::COPY),
                            "Copy",
                            kit::DIALOG_HEIGHT,
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(plan.clone());
                    }
                    if kit::button(ui, Tone::Secondary, None, "Close", kit::DIALOG_HEIGHT).clicked()
                    {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            state.explain = None;
        }
    }
    if let Some(paste) = &mut state.paste {
        let mut close = false;
        let modal =
            kit::modal(ctx, egui::Id::unique(("aggregate_paste", tab.id))).show(ctx, |ui| {
                kit::dialog_width(ui, 560.0);
                kit::dialog_title(
                    ui,
                    "Paste pipeline",
                    Some("Replaces the stages, e.g. with a pipeline from mongosh"),
                );
                let language = QueryLanguage::new(Mode::Pipeline, &[], &[]);
                let output = paste.editor.show(ui, &mut paste.text, &language);
                output.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Pipeline")
                });
                if let Some(error) = &paste.error {
                    ui.label(
                        RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT))
                            .color(theme::palette(ui).danger),
                    );
                }
                kit::dialog_footer(ui, |ui| {
                    if kit::button(
                        ui,
                        Tone::Primary,
                        None,
                        "Replace stages",
                        kit::DIALOG_HEIGHT,
                    )
                    .clicked()
                        || output.submitted
                    {
                        action = Some(AggregateAction::Paste(paste.text.clone()));
                    }
                    if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT)
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            state.paste = None;
        }
    }
    action
}

/// Six dots to drag a card by.
fn grip(ui: &mut egui::Ui) {
    let palette = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 22.0), Sense::hover());
    for row in 0..3 {
        for column in 0..2 {
            let center = egui::pos2(
                rect.left() + 3.5 + column as f32 * 5.0,
                rect.center().y - 5.0 + row as f32 * 5.0,
            );
            ui.painter().circle_filled(center, 1.4, palette.subtle);
        }
    }
}

/// A square button with three dots, for a menu of more actions.
fn dots_button(ui: &mut egui::Ui, label: &str, size: f32) -> egui::Response {
    let palette = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), palette.hover);
    }
    for step in [-5.0, 0.0, 5.0] {
        let center = rect.center() + egui::vec2(step, 0.0);
        ui.painter().circle_filled(center, 1.6, palette.muted);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    response.on_hover_text(label)
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Key, Modifiers};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::connections::Selection;

    struct State {
        tab: CollectionTab,
        settings: AggregateSettings,
        ran: bool,
    }

    fn harness() -> Harness<'static, State> {
        let tab = CollectionTab::new(
            1,
            Selection {
                connection: Uuid::nil(),
                database: "shop".to_owned(),
                collection: "orders".to_owned(),
            },
        );
        Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, state: &mut State| {
                    let surroundings = Surroundings {
                        collections: &[],
                        settings: &mut state.settings,
                        references: None,
                        read_only: false,
                        ai: false,
                    };
                    if let Some(AggregateAction::Run) = show(ui, &mut state.tab, surroundings) {
                        state.ran = true;
                    }
                },
                State {
                    tab,
                    settings: AggregateSettings::default(),
                    ran: false,
                },
            )
    }

    #[test]
    fn adds_turns_off_and_collapses_stages() {
        let mut harness = harness();
        harness.run();
        harness.get_by_label("Add stage").click();
        harness.run();
        harness.get_by_label("$group").click();
        harness.run();
        let stages = &harness.state().tab.aggregate.stages;
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].name, "$group");
        assert_eq!(stages[0].body, "{ _id:  }");

        harness.get_by_label("Turn the stage off").click();
        harness.run();
        assert!(!harness.state().tab.aggregate.stages[0].enabled);
        assert!(
            harness
                .query_by_label("Off: the pipeline skips this stage.")
                .is_some()
        );

        harness.get_by_label("Collapse the stage").click();
        harness.run();
        assert!(harness.state().tab.aggregate.stages[0].collapsed);
        assert!(harness.query_by_label("Stage 1 $group").is_none());
    }

    #[test]
    fn the_stage_picker_searches_and_renames() {
        let mut harness = harness();
        harness.state_mut().tab.aggregate.insert(0, "$match");
        harness.run();
        harness
            .get_by_label(&format!("$match  {}", icons::CHEVRON_DOWN))
            .click();
        harness.run();
        harness.get_by_label("Search stages").type_text("unw");
        harness.run();
        assert!(harness.query_by_label("$group").is_none());
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().tab.aggregate.stages[0].name, "$unwind");
    }

    #[test]
    fn ctrl_enter_in_a_stage_runs_the_pipeline() {
        let mut harness = harness();
        harness.state_mut().tab.aggregate.insert(0, "$limit");
        harness.run();
        harness.get_by_label("Stage 1 $limit").focus();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
        harness.run();
        assert!(harness.state().ran);
        assert_eq!(harness.state().tab.aggregate.stages[0].body, "10");
    }
}
