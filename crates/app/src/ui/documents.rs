//! A collection tab: query bar, result toolbar, and the list, JSON and table
//! views of the current page.

use bson::{Bson, Document, doc};
use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{
    self, Align, CornerRadius, Frame, Label, Layout, Margin, RichText, Sense, Stroke, StrokeKind,
    TextEdit,
};
use egui_extras::{Column, TableBuilder};
use nordpil_backend::SavedQuery;
use nordpil_query::{Highlight, Mode, Segment, Style, format_document, format_value};
use uuid::Uuid;

use crate::icons;
use crate::lookup::{self, References};
use crate::query_language::QueryLanguage;
use crate::tabs::{CollectionTab, PAGE_SIZES, QueryInputs, Results, View};
use crate::theme;
use crate::widgets::code_editor::{self, CodeEditor};
use crate::widgets::kit::{self, Tone};

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
    Export,
    Import,
    /// Put `path: value` into the filter and focus it, without running.
    /// [`show`] does this itself; it never reaches the app.
    AddToFilter {
        path: String,
        value: Bson,
    },
    /// Open another collection of the same database in a new tab, filtered
    /// to the documents a field refers to.
    Lookup {
        collection: String,
        filter: Document,
    },
    /// Link fields named this to a collection, or edit their link.
    LinkField(String),
}

/// The table shows at most this many columns, so huge documents stay usable.
const MAX_TABLE_COLUMNS: usize = 60;
/// An expanded object or array in the list view shows this many entries.
const MAX_CHILDREN: usize = 200;
/// Nested values with more entries get no hover preview in the table.
const MAX_HOVER_ENTRIES: usize = 50;

/// `read_only` hides everything that writes. `references` says where id
/// fields can be looked up; `None` when lookups are off.
pub fn show(
    ui: &mut egui::Ui,
    tab: &mut CollectionTab,
    read_only: bool,
    references: Option<&References>,
) -> Option<TabAction> {
    let palette = theme::palette(ui);
    let mut action = query_bar(ui, tab);
    for error in &tab.input_errors {
        ui.label(RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT)).color(palette.danger));
    }
    ui.add_space(10.0);
    action = toolbar(ui, tab, read_only).or(action);
    ui.add_space(10.0);

    if let Some(error) = &tab.error {
        ui.label(RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT)).color(palette.danger));
        return action;
    }
    let Some(results) = &tab.results else {
        if tab.is_loading() {
            ui.spinner();
        }
        return action;
    };
    if results.documents.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(icons::FILE_TEXT)
                    .size(28.0)
                    .color(palette.subtle),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(if tab.page == 0 {
                    "No documents match."
                } else {
                    "No more documents."
                })
                .color(palette.muted),
            );
        });
        return action;
    }
    let mut view_action = None;
    let page = DocumentPage {
        results,
        scope: egui::Id::unique(("tab", tab.id)),
        page: tab.page,
    };
    page_view(ui, tab.view, &page, read_only, references, &mut view_action);
    if let Some(TabAction::AddToFilter { path, value }) = &view_action {
        let edit = nordpil_query::set_field(&tab.inputs.filter, path, value);
        let cursor = edit.text[..edit.cursor].chars().count();
        tab.inputs.filter = edit.text;
        tab.editors.filter.focus_at(ui.ctx(), cursor);
        return action;
    }
    view_action.or(action)
}

/// Where a value sits in its document, for the field menu.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    /// Dotted path for a filter, without array indexes: `items.sku`.
    path: String,
    /// The nearest field name; an array item has its array's.
    name: String,
}

impl Place {
    fn root(key: &str) -> Self {
        Self {
            path: key.to_owned(),
            name: key.to_owned(),
        }
    }

    fn child(&self, key: &str) -> Self {
        Self {
            path: format!("{}.{key}", self.path),
            name: key.to_owned(),
        }
    }
}

/// The value at `segments` in `document`, and its place.
fn resolve<'a>(document: &'a Document, segments: &[Segment]) -> Option<(Place, &'a Bson)> {
    let mut found: Option<(Place, &Bson)> = None;
    for segment in segments {
        found = Some(match (segment, found) {
            (Segment::Key(key), None) => (Place::root(key), document.get(key)?),
            (Segment::Key(key), Some((place, Bson::Document(inner)))) => {
                (place.child(key), inner.get(key)?)
            }
            (Segment::Index(index), Some((place, Bson::Array(items)))) => {
                (place, items.get(*index)?)
            }
            _ => return None,
        });
    }
    found
}

/// The documents `value` refers to, by collection: where the user linked
/// its field, and collections whose names match the field's.
fn lookups<'a>(
    place: &Place,
    value: &Bson,
    references: &'a References,
) -> Vec<(&'a str, Document)> {
    references
        .targets(&place.name)
        .into_iter()
        .filter_map(|target| Some((target.collection, lookup::filter(value, target.field)?)))
        .collect()
}

/// Add to filter, exclude from filter, lookups and linking for a
/// right-clicked value. Returns true when it showed anything.
fn field_menu(
    ui: &mut egui::Ui,
    place: &Place,
    value: &Bson,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) -> bool {
    let leaf = !matches!(value, Bson::Document(_) | Bson::Array(_));
    if leaf {
        if ui.button("Add to filter").clicked() {
            *action = Some(TabAction::AddToFilter {
                path: place.path.clone(),
                value: value.clone(),
            });
        }
        if ui.button("Exclude from filter").clicked() {
            *action = Some(TabAction::AddToFilter {
                path: place.path.clone(),
                value: Bson::Document(doc! { "$ne": value.clone() }),
            });
        }
    }
    // An object can't be an id.
    let Some(references) = references.filter(|_| !matches!(value, Bson::Document(_))) else {
        return leaf;
    };
    if leaf {
        ui.separator();
    }
    for (collection, filter) in lookups(place, value, references) {
        if ui.button(format!("Open in {collection}")).clicked() {
            *action = Some(TabAction::Lookup {
                collection: collection.to_owned(),
                filter,
            });
        }
    }
    let link = if references.mapping(&place.name).is_some() {
        "Edit link…"
    } else {
        "Link to collection…"
    };
    if ui.button(link).clicked() {
        *action = Some(TabAction::LinkField(place.name.clone()));
    }
    true
}

/// Narrower than this, the query buttons move under the filter.
const MIN_FILTER_WIDTH: f32 = 320.0;

fn query_bar(ui: &mut egui::Ui, tab: &mut CollectionTab) -> Option<TabAction> {
    let mut action = None;
    let spacing = ui.spacing().item_spacing.x;
    let buttons = kit::button_width(ui, Tone::Secondary, Some(icons::SLIDERS), "Options")
        + kit::button_width(ui, Tone::Secondary, Some(icons::HISTORY), "History")
        + kit::button_width(ui, Tone::Ghost, None, "Reset")
        + kit::button_width(ui, Tone::Primary, Some(icons::SEARCH), "Find")
        + 4.0 * spacing;
    if ui.available_width() - buttons >= MIN_FILTER_WIDTH {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                query_buttons(ui, tab, &mut action);
                if filter_field(ui, tab) {
                    action = Some(TabAction::Run);
                }
            });
        });
    } else {
        ui.horizontal(|ui| {
            if filter_field(ui, tab) {
                action = Some(TabAction::Run);
            }
        });
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), kit::QUERY_HEIGHT),
            Layout::right_to_left(Align::Center),
            |ui| query_buttons(ui, tab, &mut action),
        );
    }
    if tab.show_options {
        ui.add_space(10.0);
        kit::card(ui).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if query_options(ui, tab) {
                action = Some(TabAction::Run);
            }
        });
    }
    action
}

/// Find, Reset, History and Options, laid out right to left.
fn query_buttons(ui: &mut egui::Ui, tab: &mut CollectionTab, action: &mut Option<TabAction>) {
    let palette = theme::palette(ui);
    let height = kit::QUERY_HEIGHT;
    if kit::button(ui, Tone::Primary, Some(icons::SEARCH), "Find", height).clicked() {
        *action = Some(TabAction::Run);
    }
    if kit::button(ui, Tone::Ghost, None, "Reset", height).clicked() {
        *action = Some(TabAction::Reset);
    }
    let history = kit::button(ui, Tone::Secondary, Some(icons::HISTORY), "History", height);
    egui::Popup::menu(&history).show(|ui| {
        if let Some(chosen) = history_menu(ui, &tab.history) {
            *action = Some(chosen);
            ui.close();
        }
    });
    let options = kit::button(ui, Tone::Secondary, Some(icons::SLIDERS), "Options", height);
    if tab.show_options {
        ui.painter().rect_stroke(
            options.rect,
            CornerRadius::same(10),
            Stroke::new(1.0, palette.accent),
            StrokeKind::Inside,
        );
    }
    if options.clicked() {
        tab.show_options = !tab.show_options;
    }
}

/// The filter in its 40 px field: label, editor and the Ctrl+L hint.
/// Returns true when Enter submitted it.
fn filter_field(ui: &mut egui::Ui, tab: &mut CollectionTab) -> bool {
    let palette = theme::palette(ui);
    let focused = ui.memory(|m| m.has_focus(tab.editors.filter.id()));
    let mut submitted = false;
    let frame = Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(
            1.0,
            if focused {
                palette.accent
            } else {
                palette.line_strong
            },
        ))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin {
            left: 12,
            right: 8,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(kit::QUERY_HEIGHT - 2.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(icons::FUNNEL)
                        .size(15.0)
                        .color(palette.subtle),
                );
                let label = ui.label(
                    RichText::new("Filter")
                        .size(12.5)
                        .family(theme::medium())
                        .color(palette.subtle),
                );
                kit::divider(ui, 18.0);
                let keys_width = if focused { 0.0 } else { 64.0 };
                let editor_width = (ui.available_width() - keys_width).max(80.0);
                ui.allocate_ui(egui::vec2(editor_width, kit::QUERY_HEIGHT - 2.0), |ui| {
                    ui.set_width(editor_width);
                    let language = QueryLanguage::new(Mode::Filter, &tab.fields, &tab.values);
                    let output = tab
                        .editors
                        .filter
                        .show(ui, &mut tab.inputs.filter, &language);
                    output.response.labelled_by(label.id);
                    submitted = output.submitted;
                });
                if !focused {
                    kit::keys(ui, &["Ctrl", "L"]);
                }
            });
        })
        .response;
    if focused {
        ui.painter().rect_stroke(
            frame.rect.expand(1.0),
            CornerRadius::same(11),
            Stroke::new(3.0, palette.focus),
            StrokeKind::Outside,
        );
    }
    submitted
}

/// Project, sort, collation, hint, skip, limit and max time. Returns true
/// when one of them was submitted with Enter.
fn query_options(ui: &mut egui::Ui, tab: &mut CollectionTab) -> bool {
    let mut submitted = false;
    egui::Grid::new(("query_options", tab.id))
        .num_columns(2)
        .spacing([14.0, 8.0])
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
                let label = ui.label(RichText::new(name).weak());
                let output = editor.show(ui, text, &QueryLanguage::new(mode, &tab.fields, &[]));
                output.response.labelled_by(label.id);
                submitted |= output.submitted;
                ui.end_row();
            }

            ui.label("");
            ui.horizontal(|ui| {
                for (name, text, hint) in [
                    ("Skip", &mut tab.inputs.skip, "0"),
                    ("Limit", &mut tab.inputs.limit, "none"),
                    ("Max time (ms)", &mut tab.inputs.max_time_ms, "60000"),
                ] {
                    let label = ui.label(RichText::new(name).weak());
                    let response = ui
                        .add(
                            TextEdit::singleline(text)
                                .hint_text(hint)
                                .desired_width(80.0),
                        )
                        .labelled_by(label.id);
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submitted = true;
                    }
                    ui.add_space(10.0);
                }
            });
            ui.end_row();
        });
    submitted
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
            kit::section_label(ui, section);
            heading = Some(section);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let palette = theme::palette(ui);
            let (star, color) = if saved.favorite {
                ("Remove from favorites", palette.warn)
            } else {
                ("Add to favorites", palette.subtle)
            };
            let response = ui
                .add(
                    egui::Button::new(RichText::new(icons::STAR).size(14.0).color(color))
                        .frame_when_inactive(false)
                        .min_size(egui::vec2(26.0, 26.0)),
                )
                .on_hover_text(star);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, star));
            if response.clicked() {
                action = Some(TabAction::SetFavorite {
                    id: saved.id,
                    favorite: !saved.favorite,
                });
            }
            if kit::icon_button(ui, icons::TRASH, "Delete query", 26.0).clicked() {
                action = Some(TabAction::DeleteQuery(saved.id));
            }
            ui.add_space(4.0);
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

const VIEWS: [(View, Option<&str>, &str); 3] = [
    (View::List, Some(icons::LIST), "List"),
    (View::Json, Some(icons::BRACES), "JSON"),
    (View::Table, Some(icons::TABLE), "Table"),
];

/// How much of the toolbar fits on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    OneRow,
    /// Paging moves to a second row.
    TwoRows,
    /// Two rows, and the actions show only their icons.
    Icons,
    /// Two rows, and the view switch also shows only icons.
    Narrow,
}

/// The view switch, the actions, then paging on the right; wraps onto a
/// second row and drops labels when the window is narrow.
fn toolbar(ui: &mut egui::Ui, tab: &mut CollectionTab, read_only: bool) -> Option<TabAction> {
    let mut action = None;
    let height = kit::TOOLBAR_HEIGHT;
    let actions = toolbar_actions(read_only);
    let spacing = ui.spacing().item_spacing.x;
    let labelled: f32 = actions
        .iter()
        .map(|(_, icon, label)| kit::button_width(ui, Tone::Ghost, Some(icon), label) + spacing)
        .sum();
    let icons_only = actions.len() as f32 * (height + spacing);
    let switch = |labels| kit::segmented_width(ui, &VIEWS, labels) + 9.0 + 3.0 * spacing;
    let paging = paging_width(ui, tab);
    let available = ui.available_width();
    let fit = if available >= switch(true) + labelled + paging + 24.0 {
        Fit::OneRow
    } else if available >= switch(true) + labelled {
        Fit::TwoRows
    } else if available >= switch(true) + icons_only {
        Fit::Icons
    } else {
        Fit::Narrow
    };

    ui.horizontal(|ui| {
        kit::segmented_with_labels(ui, &mut tab.view, &VIEWS, height + 2.0, fit != Fit::Narrow);
        kit::divider(ui, 22.0);
        for (kind, icon, label) in actions {
            let response = if matches!(fit, Fit::OneRow | Fit::TwoRows) {
                kit::button(ui, Tone::Ghost, Some(icon), label, height)
            } else {
                kit::icon_button(ui, icon, label, height)
            };
            match kind {
                ToolbarAction::Bulk => {
                    egui::Popup::menu(&response).show(|ui| {
                        if ui.button("Update matching…").clicked() {
                            action = Some(TabAction::UpdateMany);
                        }
                        if ui.button("Delete matching…").clicked() {
                            action = Some(TabAction::DeleteMany);
                        }
                    });
                }
                _ if response.clicked() => {
                    action = Some(match kind {
                        ToolbarAction::Insert => TabAction::Insert,
                        ToolbarAction::Import => TabAction::Import,
                        _ => TabAction::Export,
                    });
                }
                _ => {}
            }
        }
        if tab.is_loading() {
            ui.spinner();
        }
        if fit == Fit::OneRow {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                action = paging_controls(ui, tab).or(action.take());
            });
        }
    });
    if fit != Fit::OneRow {
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            Layout::right_to_left(Align::Center),
            |ui| {
                action = paging_controls(ui, tab).or(action.take());
            },
        );
    }
    action
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolbarAction {
    Insert,
    Import,
    Export,
    Bulk,
}

fn toolbar_actions(read_only: bool) -> Vec<(ToolbarAction, &'static str, &'static str)> {
    let all = [
        (ToolbarAction::Insert, icons::PLUS, "Insert"),
        (ToolbarAction::Import, icons::UPLOAD, "Import"),
        (ToolbarAction::Export, icons::DOWNLOAD, "Export"),
        (ToolbarAction::Bulk, icons::ZAP, "Bulk"),
    ];
    all.into_iter()
        .filter(|(kind, _, _)| !read_only || *kind == ToolbarAction::Export)
        .collect()
}

/// Refresh, page size, the range and the page buttons, laid out right to
/// left.
fn paging_controls(ui: &mut egui::Ui, tab: &mut CollectionTab) -> Option<TabAction> {
    let palette = theme::palette(ui);
    let mut action = None;
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.add_enabled_ui(tab.has_next_page(), |ui| {
        if page_button(ui, icons::CHEVRON_RIGHT, "Next page").clicked() {
            action = Some(TabAction::Page(tab.page + 1));
        }
    });
    ui.add_enabled_ui(tab.page > 0, |ui| {
        if page_button(ui, icons::CHEVRON_LEFT, "Previous page").clicked() {
            action = Some(TabAction::Page(tab.page - 1));
        }
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(range_text(tab))
            .size(13.0)
            .color(palette.muted),
    );
    ui.add_space(8.0);

    let before = tab.page_size;
    let combo = egui::ComboBox::from_id_salt(("page_size", tab.id))
        .width(56.0)
        .selected_text(tab.page_size.to_string())
        .show_ui(ui, |ui| {
            for size in PAGE_SIZES {
                ui.selectable_value(&mut tab.page_size, size, size.to_string());
            }
        });
    let label = ui.label(RichText::new("Per page").size(13.0).color(palette.subtle));
    combo.response.labelled_by(label.id);
    if tab.page_size != before {
        action = Some(TabAction::Page(0));
    }
    if kit::icon_button(ui, icons::REFRESH, "Refresh", kit::TOOLBAR_HEIGHT).clicked() {
        action = Some(TabAction::Refresh);
    }
    action
}

/// About the width [`paging_controls`] takes.
fn paging_width(ui: &egui::Ui, tab: &CollectionTab) -> f32 {
    let range = kit::text_width(ui, &range_text(tab), egui::FontId::proportional(13.0));
    let per_page = kit::text_width(ui, "Per page", egui::FontId::proportional(13.0));
    // Page buttons, the page size box, refresh, and the gaps between them.
    32.0 * 2.0 + range + per_page + 100.0 + kit::TOOLBAR_HEIGHT + 6.0 + 8.0 + 4.0 * 6.0
}

/// A bordered square button for paging.
fn page_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    let palette = theme::palette(ui);
    let response = kit::icon_button(ui, icon, label, 32.0);
    ui.painter().rect_stroke(
        response.rect,
        CornerRadius::same(8),
        Stroke::new(1.0, palette.line),
        StrokeKind::Inside,
    );
    response
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

pub fn thousands(n: u64) -> String {
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

/// A page of documents to show, and where its widget state lives.
pub struct DocumentPage<'a> {
    pub results: &'a Results,
    /// Keeps widget state, like expanded fields and column widths, apart
    /// from other result sets.
    pub scope: egui::Id,
    /// Expanded fields reset per page; column widths don't.
    pub page: u64,
}

/// Shows `page` in the list, JSON or table view. `read_only` hides
/// everything that writes.
pub fn page_view(
    ui: &mut egui::Ui,
    view: View,
    page: &DocumentPage,
    read_only: bool,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) {
    match view {
        View::List => list_view(ui, page, read_only, references, action),
        View::Json => json_view(ui, page, read_only, references, action),
        View::Table => table_view(ui, page, read_only, references, action),
    }
}

fn list_view(
    ui: &mut egui::Ui,
    page: &DocumentPage,
    read_only: bool,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) {
    let results = page.results;
    let palette = theme::palette(ui);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, document) in results.documents.iter().enumerate() {
                let id = page.scope.with(("document", page.page, index));
                // The scope's click area is registered before the fields, so
                // the expand arrows inside stay clickable.
                let response = ui
                    .scope_builder(
                        egui::UiBuilder::new().id_salt(id).sense(Sense::click()),
                        |ui| {
                            kit::card(ui)
                                .inner_margin(Margin::symmetric(16, 12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.spacing_mut().interact_size.y = 20.0;
                                    for (key, value) in document {
                                        let place = Place::root(key);
                                        let field_id = id.with(key);
                                        field(ui, field_id, key, value, &place, references, action);
                                    }
                                });
                        },
                    )
                    .response;
                if ui.rect_contains_pointer(response.rect) {
                    ui.painter().rect_stroke(
                        response.rect,
                        CornerRadius::same(12),
                        Stroke::new(1.0, palette.line_strong),
                        StrokeKind::Inside,
                    );
                    if !read_only {
                        card_actions(ui, response.rect, index, action);
                    }
                }
                if response.double_clicked() && !read_only {
                    *action = Some(TabAction::Edit(index));
                }
                document_menu(&response, document, index, read_only, None, action);
                ui.add_space(10.0);
            }
        });
}

/// Edit, clone and delete buttons over a hovered card's top right corner.
fn card_actions(ui: &mut egui::Ui, card: egui::Rect, index: usize, action: &mut Option<TabAction>) {
    let palette = theme::palette(ui);
    let rect = egui::Rect::from_min_size(
        egui::pos2(card.right() - 112.0, card.top() + 10.0),
        egui::vec2(102.0, 36.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    Frame::new()
        .fill(palette.raised)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(3))
        .show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if kit::icon_button(ui, icons::PENCIL, "Edit", 28.0).clicked() {
                *action = Some(TabAction::Edit(index));
            }
            if kit::icon_button(ui, icons::COPY, "Clone", 28.0).clicked() {
                *action = Some(TabAction::Clone(index));
            }
            let delete = ui
                .add(
                    egui::Button::new(RichText::new(icons::TRASH).size(14.0).color(palette.danger))
                        .frame_when_inactive(false)
                        .min_size(egui::vec2(28.0, 28.0))
                        .corner_radius(CornerRadius::same(8)),
                )
                .on_hover_text("Delete");
            delete.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Delete"));
            if delete.clicked() {
                *action = Some(TabAction::Delete(index));
            }
        });
}

/// The menu of a document. `field` is the value that was right-clicked, if
/// any; its [`field_menu`] goes first.
fn document_menu(
    response: &egui::Response,
    document: &Document,
    index: usize,
    read_only: bool,
    field: Option<(&Place, &Bson, Option<&References>)>,
    action: &mut Option<TabAction>,
) {
    response.context_menu(|ui| {
        if let Some((place, value, references)) = field
            && field_menu(ui, place, value, references, action)
        {
            ui.separator();
        }
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

/// One field of the list view; documents and arrays expand. `place` is where
/// it is, for its menu.
fn field(
    ui: &mut egui::Ui,
    id: egui::Id,
    key: &str,
    value: &Bson,
    place: &Place,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) {
    let palette = theme::palette(ui);
    let children: Option<(String, Vec<(String, &Bson)>)> = match value {
        Bson::Document(document) => Some((
            format!("{{{}}}", plural(document.len(), "field")),
            document.iter().map(|(k, v)| (k.clone(), v)).collect(),
        )),
        Bson::Array(items) => Some((
            format!("[{}]", plural(items.len(), "item")),
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
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            let key = ui.add(Label::new(key_text(ui, key)).sense(Sense::click()));
            ui.add_space(6.0);
            let value_response = value_label(ui, value, Sense::click());
            (key | value_response).context_menu(|ui| {
                if field_menu(ui, place, value, references, action) {
                    ui.separator();
                }
                if ui.button("Copy value").clicked() {
                    ui.ctx().copy_text(copy_text(value));
                }
            });
        });
        return;
    };
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let open = state.is_open();
    let header = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let chevron = ui
            .add(
                Label::new(
                    RichText::new(if open {
                        icons::CHEVRON_DOWN
                    } else {
                        icons::CHEVRON_RIGHT
                    })
                    .size(13.0)
                    .color(palette.subtle),
                )
                .sense(Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let chevron_width = chevron.rect.width();
        ui.add_space(18.0 - chevron_width);
        let label = ui.add(Label::new(key_text(ui, key)).sense(Sense::click()));
        ui.add_space(6.0);
        let summary = ui.label(
            RichText::new(summary)
                .monospace()
                .italics()
                .color(palette.subtle),
        );
        // Objects and arrays can't go into the filter, but an array of ids
        // can be looked up or linked.
        if references.is_some() && matches!(value, Bson::Array(_)) {
            (label.clone() | summary).context_menu(|ui| {
                field_menu(ui, place, value, references, action);
            });
        }
        chevron.clicked() || label.clicked()
    });
    if header.inner {
        state.set_open(!open);
        state.store(ui.ctx());
    }
    if open {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let top = ui.cursor().top();
            let body = ui
                .vertical(|ui| {
                    let total = children.len();
                    ui.add_space(0.0);
                    egui::Frame::new()
                        .inner_margin(Margin {
                            left: 12,
                            ..Margin::ZERO
                        })
                        .show(ui, |ui| {
                            for (child_key, child) in children.into_iter().take(MAX_CHILDREN) {
                                let child_place = match value {
                                    Bson::Array(_) => place.clone(),
                                    _ => place.child(&child_key),
                                };
                                let child_id = id.with(&child_key);
                                field(
                                    ui,
                                    child_id,
                                    &child_key,
                                    child,
                                    &child_place,
                                    references,
                                    action,
                                );
                            }
                            if total > MAX_CHILDREN {
                                ui.weak(format!(
                                    "… {} more. Use the JSON view or a projection to see them.",
                                    total - MAX_CHILDREN
                                ));
                            }
                        });
                })
                .response;
            ui.painter().vline(
                body.rect.left(),
                egui::Rangef::new(top, body.rect.bottom()),
                Stroke::new(1.0, palette.line),
            );
        });
    }
}

fn key_text(ui: &egui::Ui, key: &str) -> RichText {
    let color = code_editor::color(Highlight::Key, ui.visuals());
    RichText::new(format!("{key}:")).monospace().color(color)
}

/// Mono text at the list view's size.
fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

fn value_label(ui: &mut egui::Ui, value: &Bson, sense: Sense) -> egui::Response {
    let text = mono(format_value(value, Style::Compact))
        .color(code_editor::color(value_highlight(value), ui.visuals()));
    let label = Label::new(text).truncate().sense(sense);
    // A label that can't be clicked must not select text either, so clicks
    // reach what is under it.
    let label = if sense.senses_click() {
        label
    } else {
        label.selectable(false)
    };
    ui.add(label).on_hover_text(nordpil_query::type_name(value))
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
    page: &DocumentPage,
    read_only: bool,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) {
    let results = page.results;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, (preview, document)) in
                results.previews.iter().zip(&results.documents).enumerate()
            {
                let (text, spans) = (&preview.text, &preview.spans);
                let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
                    let mut job = code_editor::layout_job(ui, buffer.as_str(), spans);
                    job.wrap.max_width = wrap;
                    ui.fonts_mut(|fonts| fonts.layout_job(job))
                };
                let output = kit::card(ui)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        TextEdit::multiline(&mut text.as_str())
                            .id_salt(page.scope.with(("json", page.page, index)))
                            .frame(Frame::new())
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter)
                            .show(ui)
                    })
                    .inner;
                let response = &output.response;
                // Remember which field the menu was opened on.
                let clicked_field = response.id.with("clicked_field");
                if response.secondary_clicked()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let cursor = output.galley.cursor_from_pos(pointer - output.galley_pos);
                    let offset = text
                        .char_indices()
                        .nth(cursor.index.0)
                        .map_or(text.len(), |(offset, _)| offset);
                    let segments = nordpil_query::path_at(text, offset);
                    ui.data_mut(|data| data.insert_temp(clicked_field, segments));
                }
                let segments: Vec<Segment> = ui
                    .data(|data| data.get_temp(clicked_field))
                    .unwrap_or_default();
                let field = resolve(document, &segments);
                let field = field
                    .as_ref()
                    .map(|(place, value)| (place, *value, references));
                document_menu(response, document, index, read_only, field, action);
                if preview.full_len > text.len() {
                    ui.weak(format!(
                        "Showing the first {} of {}.",
                        human_size(text.len()),
                        human_size(preview.full_len)
                    ));
                }
                ui.add_space(10.0);
            }
        });
}

fn human_size(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn table_view(
    ui: &mut egui::Ui,
    page: &DocumentPage,
    read_only: bool,
    references: Option<&References>,
    action: &mut Option<TabAction>,
) {
    let results = page.results;
    let palette = theme::palette(ui);
    let columns = column_names(&results.documents);
    let types: Vec<&str> = columns
        .iter()
        .map(|name| column_type(&results.documents, name))
        .collect();
    kit::card(ui).inner_margin(Margin::ZERO).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::horizontal()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                kit::table_header_band(ui, 46.0);
                TableBuilder::new(ui)
                    .id_salt(page.scope.with("table"))
                    .striped(false)
                    .resizable(true)
                    .sense(Sense::click())
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .columns(
                        Column::initial(170.0).at_least(48.0).clip(true),
                        columns.len(),
                    )
                    .header(46.0, |mut header| {
                        for (name, kind) in columns.iter().zip(&types) {
                            header.col(|ui| {
                                ui.add_space(10.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(
                                        RichText::new(name.as_str())
                                            .family(theme::semibold())
                                            .size(13.0)
                                            .color(palette.text),
                                    );
                                    ui.label(mono(*kind).size(11.0).color(palette.subtle));
                                });
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(38.0, results.documents.len(), |mut row| {
                            let index = row.index();
                            let document = &results.documents[index];
                            // Remember which cell the row's menu was opened on.
                            let clicked_column = page.scope.with("table_menu_column");
                            for name in &columns {
                                let (_, cell) = row.col(|ui| {
                                    ui.add_space(10.0);
                                    if let Some(value) = document.get(name) {
                                        table_cell(ui, value);
                                    }
                                    let rect = ui.max_rect();
                                    ui.painter().hline(
                                        rect.x_range(),
                                        rect.bottom(),
                                        Stroke::new(1.0, palette.line),
                                    );
                                });
                                if cell.secondary_clicked() {
                                    cell.ctx.data_mut(|data| {
                                        data.insert_temp(clicked_column, name.clone());
                                    });
                                }
                            }
                            let response = row.response();
                            if response.double_clicked() && !read_only {
                                *action = Some(TabAction::Edit(index));
                            }
                            let column: Option<String> =
                                response.ctx.data(|data| data.get_temp(clicked_column));
                            let field = column.and_then(|name| {
                                document.get(&name).map(|value| (Place::root(&name), value))
                            });
                            let field = field
                                .as_ref()
                                .map(|(place, value)| (place, *value, references));
                            document_menu(&response, document, index, read_only, field, action);
                        });
                    });
            });
    });
}

/// The type of a column's first non-null value, e.g. `String`.
fn column_type(documents: &[Document], name: &str) -> &'static str {
    documents
        .iter()
        .filter_map(|document| document.get(name))
        .find(|value| !matches!(value, Bson::Null))
        .map_or("Null", nordpil_query::type_name)
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

/// Labels in cells only sense hovers, so right-clicks reach the cell and
/// open the row's menu with the cell's field menu.
fn table_cell(ui: &mut egui::Ui, value: &Bson) {
    let subtle = theme::palette(ui).subtle;
    let summary = |text: String| Label::new(mono(text).italics().color(subtle)).selectable(false);
    match value {
        Bson::Document(document) => {
            let label = ui.add(summary(format!("{{{}}}", plural(document.len(), "field"))));
            if document.len() <= MAX_HOVER_ENTRIES {
                label.on_hover_text(format_value(value, Style::Pretty(2)));
            }
        }
        Bson::Array(items) => {
            let label = ui.add(summary(format!("[{}]", plural(items.len(), "item"))));
            if items.len() <= MAX_HOVER_ENTRIES {
                label.on_hover_text(format_value(value, Style::Pretty(2)));
            }
        }
        _ => {
            value_label(ui, value, Sense::hover());
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
        tab.set_documents(vec![nordpil_backend::FoundDocument::new(
            doc! { "_id": 1, "address": { "city": "Oslo" } },
        )]);
        let mut harness = Harness::builder()
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, tab: &mut CollectionTab| {
                    let mut action = None;
                    let page = DocumentPage {
                        results: tab.results.as_ref().unwrap(),
                        scope: egui::Id::unique("test"),
                        page: 0,
                    };
                    list_view(ui, &page, false, None, &mut action);
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
    fn right_click_adds_a_field_to_the_filter() {
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
        tab.inputs.filter = "{ age: { $gt: 3 } }".to_owned();
        tab.set_documents(vec![nordpil_backend::FoundDocument::new(
            doc! { "_id": 1, "status": "done" },
        )]);
        let mut harness = Harness::builder()
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, tab: &mut CollectionTab| {
                    show(ui, tab, false, None);
                },
                tab,
            );
        harness.run();

        harness.get_by_label("status:").click_secondary();
        harness.run();
        harness.get_by_label("Add to filter").click();
        harness.run();

        assert_eq!(
            harness.state().inputs.filter,
            "{ age: { $gt: 3 }, status: 'done' }"
        );
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
