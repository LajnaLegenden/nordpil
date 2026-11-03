//! The shell panel: run buttons, the script editor, and the results of the
//! last run, one per statement.

use std::cell::RefCell;

use eframe::egui::{
    self, Align, Frame, Key, Layout, Margin, Modifiers, RichText, ScrollArea, TextEdit,
};
use nordpil_backend::settings::{ShellLayout, ShellOrientation};
use nordpil_shell::QueryKind;
use uuid::Uuid;

use crate::icons;
use crate::shell::{
    Body, Ending, FileJob, Selected, ShellLanguage, ShellResult, ShellState, elapsed_text,
};
use crate::tabs::CollectionTab;
use crate::theme;
use crate::ui::documents::{self, DocumentPage, VIEWS, page_button};
use crate::widgets::code_editor;
use crate::widgets::kit::{self, Tone};
use crate::widgets::split::{Pane, Split};

pub enum ShellAction {
    /// Run this script.
    Run(String),
    Stop,
    /// Show another page of a result.
    Page {
        result: usize,
        page: u64,
    },
    SetFavorite {
        id: Uuid,
        favorite: bool,
    },
    Delete(Uuid),
    /// Completion wants this collection's fields.
    Sample(String),
    File(FileJob),
    /// The layout changed for good: a drag ended, or the orientation flipped.
    SaveLayout,
}

/// What the shell panel uses besides its tab.
pub struct Surroundings<'a> {
    /// The collections of the tab's database, for completion.
    pub collections: &'a [String],
    /// Shared by every tab and kept in the settings.
    pub layout: &'a mut ShellLayout,
}

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    surroundings: Surroundings,
) -> Option<ShellAction> {
    let Surroundings {
        collections,
        layout,
    } = surroundings;
    let mut action = toolbar(ui, tab, layout);
    ui.add_space(10.0);
    let split = Split::new(
        egui::Id::unique(("shell_split", tab.id)),
        layout.orientation == ShellOrientation::SideBySide,
    )
    .default_share(layout.default_split());
    let mut editor_action = None;
    let mut results_action = None;
    let done = split.show(ui, layout.split_mut(), |ui, pane| match pane {
        Pane::First => editor_action = editor(ui, tab, collections),
        Pane::Second => results_action = results(ui, tab),
    });
    if done {
        action = action.or(Some(ShellAction::SaveLayout));
    }
    action.or(editor_action).or(results_action)
}

fn toolbar(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    layout: &mut ShellLayout,
) -> Option<ShellAction> {
    let mut action = None;
    let height = kit::TOOLBAR_HEIGHT;
    let shell = &mut tab.shell;
    ui.horizontal(|ui| {
        if shell.running.is_some() {
            if kit::button(ui, Tone::Secondary, Some(icons::SQUARE), "Stop", height).clicked() {
                action = Some(ShellAction::Stop);
            }
            ui.spinner();
        } else {
            let run = kit::button(ui, Tone::Primary, Some(icons::PLAY), "Run", height)
                .on_hover_text("Run the whole script (Ctrl+Shift+Enter)");
            if run.clicked() {
                action = Some(ShellAction::Run(shell.text.clone()));
            }
            let statement = kit::button(ui, Tone::Secondary, None, "Run statement", height)
                .on_hover_text("Run the statement at the cursor, or the selection (Ctrl+Enter)");
            if statement.clicked() {
                action = run_statement(ui.ctx(), shell);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, label, flipped) = match layout.orientation {
                ShellOrientation::Stacked => (
                    icons::COLUMNS_2,
                    "Show results beside the script",
                    ShellOrientation::SideBySide,
                ),
                ShellOrientation::SideBySide => (
                    icons::ROWS_2,
                    "Show results below the script",
                    ShellOrientation::Stacked,
                ),
            };
            if kit::icon_button(ui, icon, label, height).clicked() {
                layout.orientation = flipped;
                action = Some(ShellAction::SaveLayout);
            }
            kit::divider(ui, 22.0);
            if kit::button(ui, Tone::Ghost, Some(icons::SAVE), "Save", height)
                .on_hover_text("Save the script to a file")
                .clicked()
            {
                action = Some(ShellAction::File(FileJob::Save));
            }
            if kit::button(ui, Tone::Ghost, Some(icons::FOLDER_OPEN), "Open", height)
                .on_hover_text("Open a script file")
                .clicked()
            {
                action = Some(ShellAction::File(FileJob::Open));
            }
            let history = kit::button(ui, Tone::Ghost, Some(icons::HISTORY), "History", height);
            egui::Popup::menu(&history).show(|ui| {
                if let Some(chosen) = history_menu(ui, shell) {
                    action = Some(chosen);
                    ui.close();
                }
            });
        });
    });
    action
}

/// The statement at the cursor, or the selection, as a run.
fn run_statement(ctx: &egui::Context, shell: &ShellState) -> Option<ShellAction> {
    let selection = shell
        .editor
        .selection(ctx, &shell.text)
        .unwrap_or(shell.text.len()..shell.text.len());
    crate::shell::statement_to_run(&shell.text, selection).map(ShellAction::Run)
}

fn history_menu(ui: &mut egui::Ui, shell: &mut ShellState) -> Option<ShellAction> {
    if shell.history.is_empty() {
        ui.weak("Scripts you run show up here.");
        return None;
    }
    ui.set_max_width(480.0);
    let mut action = None;
    let mut heading = None;
    let mut chosen = None;
    for saved in &shell.history {
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
                action = Some(ShellAction::SetFavorite {
                    id: saved.id,
                    favorite: !saved.favorite,
                });
            }
            if kit::icon_button(ui, icons::TRASH, "Delete script", 26.0).clicked() {
                action = Some(ShellAction::Delete(saved.id));
            }
            ui.add_space(4.0);
            let first_line = saved.script.lines().next().unwrap_or_default();
            let summary = if saved.script.trim().contains('\n') {
                format!("{first_line} …")
            } else {
                first_line.to_owned()
            };
            let response = ui.add(
                egui::Button::new(RichText::new(summary).monospace())
                    .frame(false)
                    .truncate(),
            );
            if response.on_hover_text(saved.script.as_str()).clicked() {
                chosen = Some(saved.script.clone());
            }
        });
    }
    if let Some(script) = chosen {
        shell.text = script;
        shell.edited();
        ui.close();
    }
    action
}

fn editor(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    collections: &[String],
) -> Option<ShellAction> {
    let shell = &mut tab.shell;
    let mut action = None;
    let focused = ui.memory(|memory| memory.has_focus(shell.editor.id()));
    if focused
        && shell.running.is_none()
        && ui
            .input_mut(|input| input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter))
    {
        action = Some(ShellAction::Run(shell.text.clone()));
    }
    let language = ShellLanguage {
        collections,
        collection: &tab.target.collection,
        own_fields: &tab.fields,
        own_values: &tab.values,
        fields: &shell.fields,
        wanted: RefCell::new(None),
    };
    let before = shell.text.clone();
    let framed = code_editor::frame(ui, focused)
        .inner_margin(Margin::same(1))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt(("shell_script", tab.id))
                .auto_shrink(false)
                .show(ui, |ui| {
                    shell.editor.set_min_height(ui.available_height());
                    shell.editor.show(ui, &mut shell.text, &language)
                })
                .inner
        });
    if focused {
        code_editor::paint_focus_ring(ui, framed.response.rect);
    }
    let output = framed.inner;
    let wanted = language.wanted.into_inner();
    output
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Script"));
    if shell.text != before {
        shell.edited();
    }
    if output.submitted && shell.running.is_none() && action.is_none() {
        action = run_statement(ui.ctx(), shell);
    }
    if let Some(name) = wanted
        && !shell.sampling.contains(&name)
    {
        action = action.or(Some(ShellAction::Sample(name)));
    }
    action
}

fn results(ui: &mut egui::Ui, tab: &mut CollectionTab) -> Option<ShellAction> {
    let palette = theme::palette(ui);
    let shell = &mut tab.shell;
    match &shell.ending {
        Some(Ending::Failed(error)) => {
            let place = match (error.line, error.column) {
                (Some(line), Some(column)) => format!(" (line {line}, column {column})"),
                (Some(line), None) => format!(" (line {line})"),
                _ => String::new(),
            };
            ui.label(
                RichText::new(format!("{}  {}{place}", icons::CIRCLE_ALERT, error.message))
                    .color(palette.danger),
            );
            ui.add_space(8.0);
        }
        Some(Ending::Stopped) => {
            ui.label(RichText::new("Stopped.").color(palette.muted));
            ui.add_space(8.0);
        }
        Some(Ending::Declined) => {
            ui.label(RichText::new("Stopped: the write was not allowed.").color(palette.muted));
            ui.add_space(8.0);
        }
        Some(Ending::Empty) => {
            ui.label(
                RichText::new("Done. The script returned nothing to show.").color(palette.muted),
            );
        }
        None => {}
    }
    if shell.results.is_empty() && shell.output.is_empty() {
        if shell.running.is_none() && shell.ending.is_none() {
            ui.label(
                RichText::new(
                    "Write mongosh-style JavaScript and run it. Each statement's value shows here.",
                )
                .color(palette.muted),
            );
        }
        return None;
    }
    ScrollArea::horizontal()
        .id_salt(("shell_results_strip", tab.id))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (index, result) in shell.results.iter().enumerate() {
                    let selected = shell.selected == Some(Selected::Result(index));
                    let label = format!("{}  {}", index + 1, result.title);
                    if result_tab(ui, &label, selected).clicked() {
                        shell.selected = Some(Selected::Result(index));
                    }
                }
                if !shell.output.is_empty() {
                    let selected = shell.selected == Some(Selected::Output);
                    let label = format!("Output · {}", shell.output.len());
                    if result_tab(ui, &label, selected).clicked() {
                        shell.selected = Some(Selected::Output);
                    }
                }
            });
        });
    ui.add_space(10.0);
    match shell.selected {
        Some(Selected::Result(index)) if index < shell.results.len() => result_view(ui, tab, index),
        Some(Selected::Output) => {
            output_view(ui, &shell.output);
            None
        }
        _ => None,
    }
}

/// A rounded tab for one result, filled when selected.
fn result_tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let palette = theme::palette(ui);
    let (fill, color, stroke) = if selected {
        (palette.accent_soft, palette.accent_text, egui::Stroke::NONE)
    } else {
        (
            palette.raised,
            palette.muted,
            egui::Stroke::new(1.0, palette.line),
        )
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(13.0).color(color))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(egui::CornerRadius::same(255))
            .min_size(egui::vec2(0.0, 28.0)),
    )
}

fn result_view(ui: &mut egui::Ui, tab: &mut CollectionTab, index: usize) -> Option<ShellAction> {
    let palette = theme::palette(ui);
    let page_size = tab.page_size;
    let scope = egui::Id::unique(("shell_result", tab.id, index));
    let shell = &mut tab.shell;
    let ShellResult { elapsed, body, .. } = &shell.results[index];
    let mut action = None;
    let describe = match body {
        Body::Query(page) => {
            let (kind, namespace) = match &page.query.kind {
                QueryKind::Find { .. } => ("find", &page.query.namespace),
                QueryKind::Aggregate { .. } => ("aggregate", &page.query.namespace),
            };
            format!("{kind} · {}.{}", namespace.database, namespace.collection)
        }
        Body::Documents { documents, .. } => documents::plural(documents.len(), "document"),
        Body::Value { .. } => "value".to_owned(),
    };
    let info = format!("{describe} · {}", elapsed_text(*elapsed));
    let is_value = matches!(body, Body::Value { .. });
    ui.horizontal(|ui| {
        if !is_value {
            kit::segmented_with_labels(
                ui,
                &mut shell.view,
                &VIEWS,
                kit::TOOLBAR_HEIGHT + 2.0,
                true,
            );
            kit::divider(ui, 22.0);
        }
        ui.label(RichText::new(info).color(palette.muted));
        let (page, has_next, first, shown, loading) = match body {
            Body::Query(query) => (
                query.page,
                query.has_next(page_size),
                query.page * page_size,
                query.results.as_ref().map_or(0, |r| r.documents.len()),
                query.pending.is_some(),
            ),
            Body::Documents {
                documents,
                page,
                current,
            } => (
                *page,
                ((*page + 1) * page_size) < documents.len() as u64,
                page * page_size,
                current.documents.len(),
                false,
            ),
            Body::Value { .. } => return,
        };
        if loading {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_enabled_ui(has_next, |ui| {
                if page_button(ui, icons::CHEVRON_RIGHT, "Next page").clicked() {
                    action = Some(ShellAction::Page {
                        result: index,
                        page: page + 1,
                    });
                }
            });
            ui.add_enabled_ui(page > 0, |ui| {
                if page_button(ui, icons::CHEVRON_LEFT, "Previous page").clicked() {
                    action = Some(ShellAction::Page {
                        result: index,
                        page: page - 1,
                    });
                }
            });
            if shown > 0 {
                ui.label(
                    RichText::new(format!("{}–{}", first + 1, first + shown as u64))
                        .color(palette.muted),
                );
            }
        });
    });
    ui.add_space(10.0);
    let view = shell.view;
    let (results, page) = match &shell.results[index].body {
        Body::Query(query) => {
            if let Some(error) = &query.error {
                ui.label(
                    RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT))
                        .color(palette.danger),
                );
                return action;
            }
            match &query.results {
                Some(results) => (results, query.page),
                None => return action,
            }
        }
        Body::Documents { current, page, .. } => (current, *page),
        Body::Value { text, spans } => {
            value_view(ui, scope, text, spans);
            return action;
        }
    };
    if results.documents.is_empty() {
        ui.label(RichText::new("No documents.").color(palette.muted));
        return action;
    }
    let page = DocumentPage {
        results,
        scope,
        page,
        field_menus: false,
    };
    let mut ignored = None;
    documents::page_view(ui, view, &page, true, None, &mut ignored);
    action
}

fn value_view(ui: &mut egui::Ui, scope: egui::Id, text: &str, spans: &[code_editor::Span]) {
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = code_editor::layout_job(ui, buffer.as_str(), spans);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        kit::card(ui)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                TextEdit::multiline(&mut &*text)
                    .id_salt(scope.with("value"))
                    .frame(Frame::new())
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter)
                    .show(ui);
            });
    });
}

fn output_view(ui: &mut egui::Ui, output: &[String]) {
    let text = output.join("\n");
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        kit::card(ui)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                TextEdit::multiline(&mut text.as_str())
                    .id_salt("shell_output")
                    .font(egui::TextStyle::Monospace)
                    .frame(Frame::new())
                    .desired_width(f32::INFINITY)
                    .show(ui);
            });
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bson::doc;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::connections::Selection;
    use crate::shell::page_of;

    struct State {
        tab: CollectionTab,
        layout: ShellLayout,
        ran: Option<String>,
        saved_layout: bool,
    }

    fn tab_with_results() -> CollectionTab {
        let mut tab = CollectionTab::new(
            1,
            Selection {
                connection: Uuid::nil(),
                database: "shop".to_owned(),
                collection: "users".to_owned(),
            },
        );
        tab.shell.load_draft(None, "users");
        let documents = vec![doc! { "_id": 1, "name": "Ada" }];
        tab.shell.results.push(ShellResult {
            title: "db.users.find({}).toArray()".to_owned(),
            elapsed: Duration::from_millis(12),
            body: Body::Documents {
                current: page_of(&documents, 0, 20),
                documents,
                page: 0,
            },
        });
        tab.shell.output.push("hello".to_owned());
        tab.shell.selected = Some(Selected::Result(0));
        tab
    }

    fn shell_harness() -> Harness<'static, State> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, state: &mut State| {
                    let surroundings = Surroundings {
                        collections: &[],
                        layout: &mut state.layout,
                    };
                    match show(ui, &mut state.tab, surroundings) {
                        Some(ShellAction::Run(script)) => state.ran = Some(script),
                        Some(ShellAction::SaveLayout) => state.saved_layout = true,
                        _ => {}
                    }
                },
                State {
                    tab: tab_with_results(),
                    layout: ShellLayout::default(),
                    ran: None,
                    saved_layout: false,
                },
            )
    }

    #[test]
    fn shows_results_and_runs_the_script() {
        let mut harness = shell_harness();
        harness.run();

        assert!(
            harness
                .query_by_label("1  db.users.find({}).toArray()")
                .is_some()
        );
        assert!(harness.query_by_label("name:").is_some());

        harness.get_by_label("Output · 1").click();
        harness.run();
        assert_eq!(harness.state().tab.shell.selected, Some(Selected::Output));

        harness.get_by_label("Run").click();
        harness.run();
        assert_eq!(harness.state().ran.as_deref(), Some("db.users.find({})"));
    }

    #[test]
    fn the_layout_button_puts_results_beside_the_script_and_back() {
        let mut harness = shell_harness();
        harness.run();
        let script = harness.get_by_label("Script").rect();
        let result = harness.get_by_label("name:").rect();
        assert!(result.top() > script.bottom(), "{script:?} {result:?}");

        harness
            .get_by_label("Show results beside the script")
            .click();
        harness.run();
        assert_eq!(
            harness.state().layout.orientation,
            ShellOrientation::SideBySide
        );
        assert!(harness.state().saved_layout);
        let script = harness.get_by_label("Script").rect();
        let result = harness.get_by_label("name:").rect();
        assert!(result.left() > script.right(), "{script:?} {result:?}");

        harness.state_mut().saved_layout = false;
        harness
            .get_by_label("Show results below the script")
            .click();
        harness.run();
        assert_eq!(
            harness.state().layout.orientation,
            ShellOrientation::Stacked
        );
        assert!(harness.state().saved_layout);
    }
}
