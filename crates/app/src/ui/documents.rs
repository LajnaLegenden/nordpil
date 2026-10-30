//! A collection tab: query bar, result toolbar, and the list, JSON and table
//! views of the current page.

use bson::{Bson, Document};
use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, TextEdit};
use egui_extras::{Column, TableBuilder};
use nordpil_backend::SavedQuery;
use nordpil_query::{Highlight, Mode, Style, format_document, format_value};
use uuid::Uuid;

use crate::query_language::QueryLanguage;
use crate::tabs::{CollectionTab, PAGE_SIZES, QueryInputs, View};
use crate::widgets::code_editor::{self, CodeEditor};

pub enum TabAction {
    /// Run the query from the inputs, from the first page.
    Run,
    /// Clear the inputs, then run.
    Reset,
    /// Fetch another page of the applied query.
    Page(u64),
    /// Run the applied query again.
    Refresh,
    /// Fill in a query from the history and run it.
    Apply(QueryInputs),
    SetFavorite {
        id: Uuid,
        favorite: bool,
    },
    DeleteQuery(Uuid),
    Insert,
    /// Documents by index on the current page.
    Edit(usize),
    Clone(usize),
    Delete(usize),
    UpdateMany,
    DeleteMany,
}

/// The table shows at most this many columns, so huge documents stay usable.
const MAX_TABLE_COLUMNS: usize = 60;

/// `read_only` hides everything that writes.
pub fn show(ui: &mut egui::Ui, tab: &mut CollectionTab, read_only: bool) -> Option<TabAction> {
    let mut action = query_bar(ui, tab);
    for error in &tab.input_errors {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    ui.add_space(4.0);
    action = toolbar(ui, tab, read_only).or(action);
    ui.separator();

    if let Some(error) = &tab.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return action;
    }
    let Some(results) = &tab.results else {
        if tab.is_loading() {
            ui.spinner();
        }
        return action;
    };
    if results.documents.is_empty() {
        ui.weak(if tab.page == 0 {
            "No documents match."
        } else {
            "No more documents."
        });
        return action;
    }
    let mut view_action = None;
    match tab.view {
        View::List => list_view(ui, tab, read_only, &mut view_action),
        View::Json => json_view(ui, tab, read_only, &mut view_action),
        View::Table => table_view(ui, tab, read_only, &mut view_action),
    }
    view_action.or(action)
}

fn query_bar(ui: &mut egui::Ui, tab: &mut CollectionTab) -> Option<TabAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        let label = ui.label("Filter");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Find").clicked() {
                action = Some(TabAction::Run);
            }
            if ui.button("Reset").clicked() {
                action = Some(TabAction::Reset);
            }
            ui.toggle_value(&mut tab.show_options, "Options");
            ui.menu_button("History", |ui| {
                if let Some(chosen) = history_menu(ui, &tab.history) {
                    action = Some(chosen);
                    ui.close();
                }
            });
            let language = QueryLanguage::new(Mode::Filter, &tab.fields);
            let output = tab
                .editors
                .filter
                .show(ui, &mut tab.inputs.filter, &language);
            output.response.labelled_by(label.id);
            if output.submitted {
                action = Some(TabAction::Run);
            }
        });
    });
    if tab.show_options {
        egui::Grid::new(("query_options", tab.id))
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let editors = &mut tab.editors;
                let inputs = &mut tab.inputs;
                let rows: [(&str, &mut CodeEditor, &mut String, Mode); 4] = [
                    (
                        "Project",
                        &mut editors.project,
                        &mut inputs.project,
                        Mode::Fields,
                    ),
                    ("Sort", &mut editors.sort, &mut inputs.sort, Mode::Fields),
                    (
                        "Collation",
                        &mut editors.collation,
                        &mut inputs.collation,
                        Mode::Collation,
                    ),
                    ("Hint", &mut editors.hint, &mut inputs.hint, Mode::Fields),
                ];
                for (name, editor, text, mode) in rows {
                    let label = ui.label(name);
                    let output = editor.show(ui, text, &QueryLanguage::new(mode, &tab.fields));
                    output.response.labelled_by(label.id);
                    if output.submitted {
                        action = Some(TabAction::Run);
                    }
                    ui.end_row();
                }

                ui.label("");
                ui.horizontal(|ui| {
                    for (name, text, hint) in [
                        ("Skip", &mut tab.inputs.skip, "0"),
                        ("Limit", &mut tab.inputs.limit, "none"),
                        ("Max time (ms)", &mut tab.inputs.max_time_ms, "60000"),
                    ] {
                        let label = ui.label(name);
                        let response = ui
                            .add(
                                TextEdit::singleline(text)
                                    .hint_text(hint)
                                    .desired_width(70.0),
                            )
                            .labelled_by(label.id);
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = Some(TabAction::Run);
                        }
                        ui.add_space(8.0);
                    }
                });
                ui.end_row();
            });
    }
    action
}

fn history_menu(ui: &mut egui::Ui, history: &[SavedQuery]) -> Option<TabAction> {
    if history.is_empty() {
        ui.weak("Queries you run show up here.");
        return None;
    }
    ui.set_max_width(480.0);
    let mut action = None;
    let mut heading = None;
    for saved in history {
        let section = if saved.favorite {
            "Favorites"
        } else {
            "Recent"
        };
        if heading != Some(section) {
            if heading.is_some() {
                ui.separator();
            }
            ui.weak(section);
            heading = Some(section);
        }
        ui.horizontal(|ui| {
            let star = if saved.favorite { "Unstar" } else { "Star" };
            if ui.small_button(star).clicked() {
                action = Some(TabAction::SetFavorite {
                    id: saved.id,
                    favorite: !saved.favorite,
                });
            }
            if ui.small_button("Delete").clicked() {
                action = Some(TabAction::DeleteQuery(saved.id));
            }
            let summary = query_summary(&saved.query);
            let response = ui.add(
                egui::Button::new(RichText::new(&summary).monospace())
                    .frame(false)
                    .truncate(),
            );
            if response.on_hover_text(summary.as_str()).clicked() {
                action = Some(TabAction::Apply(saved.query.clone()));
            }
        });
    }
    action
}

/// The filter plus any options, on one line.
fn query_summary(query: &QueryInputs) -> String {
    let mut parts = vec![if query.filter.trim().is_empty() {
        "{}".to_owned()
    } else {
        query.filter.trim().to_owned()
    }];
    for (name, value) in [
        ("project", &query.project),
        ("sort", &query.sort),
        ("collation", &query.collation),
        ("hint", &query.hint),
        ("skip", &query.skip),
        ("limit", &query.limit),
        ("maxTimeMS", &query.max_time_ms),
    ] {
        if !value.trim().is_empty() {
            parts.push(format!("{name}: {}", value.trim()));
        }
    }
    parts.join("  ")
}

fn toolbar(ui: &mut egui::Ui, tab: &mut CollectionTab, read_only: bool) -> Option<TabAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        for (view, name) in [
            (View::List, "List"),
            (View::Json, "JSON"),
            (View::Table, "Table"),
        ] {
            ui.selectable_value(&mut tab.view, view, name);
        }
        if !read_only {
            ui.separator();
            if ui.button("Insert").clicked() {
                action = Some(TabAction::Insert);
            }
            ui.menu_button("Bulk", |ui| {
                if ui.button("Update matching…").clicked() {
                    action = Some(TabAction::UpdateMany);
                }
                if ui.button("Delete matching…").clicked() {
                    action = Some(TabAction::DeleteMany);
                }
            });
        }
        if tab.is_loading() {
            ui.spinner();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(tab.has_next_page(), egui::Button::new("Next"))
                .clicked()
            {
                action = Some(TabAction::Page(tab.page + 1));
            }
            if ui
                .add_enabled(tab.page > 0, egui::Button::new("Previous"))
                .clicked()
            {
                action = Some(TabAction::Page(tab.page - 1));
            }
            ui.label(range_text(tab));

            let before = tab.page_size;
            let combo = egui::ComboBox::from_id_salt(("page_size", tab.id))
                .width(56.0)
                .selected_text(tab.page_size.to_string())
                .show_ui(ui, |ui| {
                    for size in PAGE_SIZES {
                        ui.selectable_value(&mut tab.page_size, size, size.to_string());
                    }
                });
            let label = ui.label("Per page");
            combo.response.labelled_by(label.id);
            if tab.page_size != before {
                action = Some(TabAction::Page(0));
            }
            if ui.button("Refresh").clicked() {
                action = Some(TabAction::Refresh);
            }
        });
    });
    action
}

/// E.g. "21–40 of 1,234".
fn range_text(tab: &CollectionTab) -> String {
    let shown = tab.results.as_ref().map_or(0, |r| r.documents.len() as u64);
    let total = match tab.total() {
        Some(Some(total)) => thousands(total),
        Some(None) => "many".to_owned(),
        None => "…".to_owned(),
    };
    if shown == 0 {
        return format!("0 of {total}");
    }
    let first = tab.page * tab.page_size + 1;
    format!(
        "{}–{} of {total}",
        thousands(first),
        thousands(first + shown - 1)
    )
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn list_view(
    ui: &mut egui::Ui,
    tab: &CollectionTab,
    read_only: bool,
    action: &mut Option<TabAction>,
) {
    let Some(results) = &tab.results else {
        return;
    };
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, document) in results.documents.iter().enumerate() {
                let id = egui::Id::unique(("document", tab.id, tab.page, index));
                // The scope's click area is registered before the fields, so
                // the expand arrows inside stay clickable.
                let response = ui
                    .scope_builder(
                        egui::UiBuilder::new().id_salt(id).sense(Sense::click()),
                        |ui| {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                for (key, value) in document {
                                    field(ui, id.with(key), key, value);
                                }
                            });
                        },
                    )
                    .response;
                if response.double_clicked() && !read_only {
                    *action = Some(TabAction::Edit(index));
                }
                document_menu(&response, document, index, read_only, action);
                ui.add_space(4.0);
            }
        });
}

fn document_menu(
    response: &egui::Response,
    document: &Document,
    index: usize,
    read_only: bool,
    action: &mut Option<TabAction>,
) {
    response.context_menu(|ui| {
        if ui.button("Copy document").clicked() {
            ui.ctx()
                .copy_text(format_document(document, Style::Pretty(2)));
        }
        if read_only {
            return;
        }
        ui.separator();
        if ui.button("Edit…").clicked() {
            *action = Some(TabAction::Edit(index));
        }
        if ui.button("Clone…").clicked() {
            *action = Some(TabAction::Clone(index));
        }
        if ui.button("Delete…").clicked() {
            *action = Some(TabAction::Delete(index));
        }
    });
}

/// One field of the list view; documents and arrays expand.
fn field(ui: &mut egui::Ui, id: egui::Id, key: &str, value: &Bson) {
    let children: Option<(String, Vec<(String, &Bson)>)> = match value {
        Bson::Document(document) => Some((
            format!("{{ {} }}", plural(document.len(), "field")),
            document.iter().map(|(k, v)| (k.clone(), v)).collect(),
        )),
        Bson::Array(items) => Some((
            format!("[ {} ]", plural(items.len(), "item")),
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v))
                .collect(),
        )),
        _ => None,
    };
    let Some((summary, children)) = children else {
        ui.horizontal(|ui| {
            key_label(ui, key);
            let response = value_label(ui, value);
            response.context_menu(|ui| {
                if ui.button("Copy value").clicked() {
                    ui.ctx().copy_text(copy_text(value));
                }
            });
        });
        return;
    };
    let (_, header, _) = CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            let label = ui.add(Label::new(key_text(ui, key)).sense(Sense::click()));
            ui.weak(summary);
            label
        })
        .body(|ui| {
            for (child_key, child) in children {
                field(ui, id.with(&child_key), &child_key, child);
            }
        });
    // Clicking the field name expands it too, not only the arrow.
    if header.inner.clicked()
        && let Some(mut state) = CollapsingState::load(ui.ctx(), id)
    {
        state.set_open(!state.is_open());
        state.store(ui.ctx());
    }
}

fn key_label(ui: &mut egui::Ui, key: &str) {
    ui.label(key_text(ui, key));
}

fn key_text(ui: &egui::Ui, key: &str) -> RichText {
    let color = code_editor::color(Highlight::Key, ui.visuals());
    RichText::new(format!("{key}:")).monospace().color(color)
}

fn value_label(ui: &mut egui::Ui, value: &Bson) -> egui::Response {
    let text = RichText::new(format_value(value, Style::Compact))
        .monospace()
        .color(code_editor::color(value_highlight(value), ui.visuals()));
    ui.add(Label::new(text).truncate().sense(Sense::click()))
        .on_hover_text(nordpil_query::type_name(value))
}

/// Strings copy without quotes; everything else as shell syntax.
fn copy_text(value: &Bson) -> String {
    match value {
        Bson::String(text) => text.clone(),
        _ => format_value(value, Style::Pretty(2)),
    }
}

fn json_view(
    ui: &mut egui::Ui,
    tab: &CollectionTab,
    read_only: bool,
    action: &mut Option<TabAction>,
) {
    let Some(results) = &tab.results else {
        return;
    };
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, ((text, spans), document)) in
                results.json.iter().zip(&results.documents).enumerate()
            {
                let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
                    let mut job = code_editor::layout_job(ui, buffer.as_str(), spans);
                    job.wrap.max_width = wrap;
                    ui.fonts_mut(|fonts| fonts.layout_job(job))
                };
                let response = ui.add(
                    TextEdit::multiline(&mut text.as_str())
                        .id_salt(("json", tab.id, tab.page, index))
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter),
                );
                document_menu(&response, document, index, read_only, action);
            }
        });
}

fn table_view(
    ui: &mut egui::Ui,
    tab: &CollectionTab,
    read_only: bool,
    action: &mut Option<TabAction>,
) {
    let Some(results) = &tab.results else {
        return;
    };
    let columns = column_names(&results.documents);
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
    egui::ScrollArea::horizontal()
        .auto_shrink(false)
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt(("table", tab.id))
                .striped(true)
                .resizable(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .columns(
                    Column::initial(160.0).at_least(40.0).clip(true),
                    columns.len(),
                )
                .header(row_height + 2.0, |mut header| {
                    for name in &columns {
                        header.col(|ui| {
                            ui.strong(name.as_str());
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, results.documents.len(), |mut row| {
                        let index = row.index();
                        let document = &results.documents[index];
                        for name in &columns {
                            let (_, response) = row.col(|ui| {
                                if let Some(value) = document.get(name) {
                                    table_cell(ui, value);
                                }
                            });
                            let response = response.interact(Sense::click());
                            if response.double_clicked() && !read_only {
                                *action = Some(TabAction::Edit(index));
                            }
                            document_menu(&response, document, index, read_only, action);
                        }
                    });
                });
        });
}

/// `_id` first, then other fields in the order they first appear.
fn column_names(documents: &[Document]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if documents.iter().any(|d| d.contains_key("_id")) {
        names.push("_id".to_owned());
    }
    for document in documents {
        for key in document.keys() {
            if names.len() >= MAX_TABLE_COLUMNS {
                return names;
            }
            if !names.iter().any(|name| name == key) {
                names.push(key.clone());
            }
        }
    }
    names
}

fn table_cell(ui: &mut egui::Ui, value: &Bson) {
    match value {
        Bson::Document(document) => {
            ui.weak(format!("{{ {} }}", plural(document.len(), "field")))
                .on_hover_text(format_value(value, Style::Pretty(2)));
        }
        Bson::Array(items) => {
            ui.weak(format!("[ {} ]", plural(items.len(), "item")))
                .on_hover_text(format_value(value, Style::Pretty(2)));
        }
        _ => {
            // The row's menu (copy/edit/delete) covers the cell.
            value_label(ui, value);
        }
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn value_highlight(value: &Bson) -> Highlight {
    match value {
        Bson::String(_) | Bson::Symbol(_) => Highlight::String,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            Highlight::Number
        }
        Bson::Boolean(_) | Bson::Null | Bson::Undefined | Bson::MinKey | Bson::MaxKey => {
            Highlight::Keyword
        }
        Bson::RegularExpression(_) => Highlight::Regex,
        _ => Highlight::Function,
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn columns_start_with_id_then_first_seen_order() {
        let documents = [doc! { "b": 1, "_id": 1 }, doc! { "a": 1, "b": 2 }];
        assert_eq!(column_names(&documents), ["_id", "b", "a"]);
    }

    #[test]
    fn nested_fields_expand_on_click() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable as _;

        let mut tab = CollectionTab::new(
            1,
            crate::connections::Selection {
                connection: uuid::Uuid::nil(),
                database: "db".to_owned(),
                collection: "c".to_owned(),
            },
        );
        tab.set_documents(vec![doc! { "_id": 1, "address": { "city": "Oslo" } }]);
        let mut harness = Harness::new_ui_state(
            |ui, tab: &mut CollectionTab| {
                let mut action = None;
                list_view(ui, tab, false, &mut action);
            },
            tab,
        );
        harness.run();
        assert!(harness.query_by_label("city:").is_none());

        // Click the expand arrow just left of the field name, like a user would.
        let label = harness.get_by_label("address:").rect();
        let arrow = egui::pos2(label.left() - 8.0, label.center().y);
        harness.hover_at(arrow);
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: arrow,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.run();
        assert!(harness.query_by_label("city:").is_some());

        // The field name toggles too.
        harness.get_by_label("address:").click();
        harness.run();
        assert!(harness.query_by_label("city:").is_none());
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
