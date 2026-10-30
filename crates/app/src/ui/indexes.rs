//! A collection's indexes: keys, properties, size and usage, with actions.

use bson::{Bson, Document};
use eframe::egui::{self, Align, Layout};
use egui_extras::{Column, TableBuilder};
use nordpil_backend::IndexInfo;
use nordpil_query::{Style, format_document, format_value};

use crate::tabs::CollectionTab;

pub enum IndexAction {
    Refresh,
    Create,
    /// Asks for confirmation first.
    Drop(String),
    SetHidden {
        name: String,
        hidden: bool,
    },
}

pub fn show(ui: &mut egui::Ui, tab: &CollectionTab, read_only: bool) -> Option<IndexAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if !read_only && ui.button("Create index").clicked() {
            action = Some(IndexAction::Create);
        }
        if ui.button("Refresh").clicked() {
            action = Some(IndexAction::Refresh);
        }
        if tab.indexes_request.is_some() {
            ui.spinner();
        }
    });
    ui.separator();

    let indexes = match &tab.indexes {
        None => return action,
        Some(Err(error)) => {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return action;
        }
        Some(Ok(indexes)) => indexes,
    };
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt(("indexes", tab.id))
            .striped(true)
            .resizable(true)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(150.0).at_least(80.0).clip(true))
            .column(Column::initial(200.0).at_least(80.0).clip(true))
            .column(Column::initial(140.0).at_least(80.0).clip(true))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(90.0).at_least(60.0).clip(true))
            .column(Column::remainder().at_least(110.0))
            .header(row_height, |mut header| {
                for name in ["Name", "Keys", "Properties", "Size", "Usage", ""] {
                    header.col(|ui| {
                        ui.strong(name);
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, indexes.len(), |mut row| {
                    let index = &indexes[row.index()];
                    row.col(|ui| {
                        ui.label(&index.name);
                    });
                    row.col(|ui| {
                        ui.monospace(format_document(&index.keys(), Style::Compact));
                    });
                    row.col(|ui| {
                        let properties = properties(index);
                        ui.label(properties.join(", "))
                            .on_hover_text(format_document(&index.spec, Style::Pretty(2)));
                    });
                    row.col(|ui| {
                        ui.label(index.size.map_or_else(|| "–".to_owned(), human_bytes));
                    });
                    row.col(|ui| {
                        let label = ui.label(
                            index
                                .ops
                                .map_or_else(|| "–".to_owned(), |ops| format!("{ops} uses")),
                        );
                        if let Some(since) = index.since {
                            label.on_hover_text(format!(
                                "Since {}",
                                format_value(&Bson::DateTime(since), Style::Compact)
                            ));
                        }
                    });
                    row.col(|ui| {
                        if read_only || index.is_id_index() {
                            return;
                        }
                        let hidden = index.spec.get_bool("hidden").unwrap_or(false);
                        let toggle = if hidden { "Unhide" } else { "Hide" };
                        if ui
                            .small_button(toggle)
                            .on_hover_text(
                                "Hidden indexes stay up to date but queries don't use them.",
                            )
                            .clicked()
                        {
                            action = Some(IndexAction::SetHidden {
                                name: index.name.clone(),
                                hidden: !hidden,
                            });
                        }
                        if ui.small_button("Drop").clicked() {
                            action = Some(IndexAction::Drop(index.name.clone()));
                        }
                    });
                });
            });
    });
    action
}

/// Short labels for the index's options, e.g. "unique, TTL 3600 s".
fn properties(index: &IndexInfo) -> Vec<String> {
    let spec = &index.spec;
    let mut properties = Vec::new();
    let keys = index.keys();
    for kind in key_kinds(&keys) {
        properties.push(kind);
    }
    let flag = |name: &str| spec.get_bool(name).unwrap_or(false);
    if flag("unique") {
        properties.push("unique".to_owned());
    }
    if flag("sparse") {
        properties.push("sparse".to_owned());
    }
    if let Some(seconds) = spec.get("expireAfterSeconds") {
        properties.push(format!("TTL {} s", format_value(seconds, Style::Compact)));
    }
    if spec.contains_key("partialFilterExpression") {
        properties.push("partial".to_owned());
    }
    if spec.contains_key("collation") {
        properties.push("collation".to_owned());
    }
    if flag("hidden") {
        properties.push("hidden".to_owned());
    }
    properties
}

/// Special index types from string key values, e.g. `text` or `2dsphere`.
fn key_kinds(keys: &Document) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for value in keys.values() {
        if let Bson::String(kind) = value
            && !kinds.contains(kind)
        {
            kinds.push(kind.clone());
        }
    }
    kinds
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn describes_index_properties() {
        let index = IndexInfo {
            name: "x".to_owned(),
            spec: doc! {
                "key": { "body": "text", "n": 1 },
                "unique": true,
                "expireAfterSeconds": 3600,
                "partialFilterExpression": { "n": { "$gt": 1 } },
                "hidden": true,
            },
            size: None,
            ops: None,
            since: None,
        };
        assert_eq!(
            properties(&index),
            ["text", "unique", "TTL 3600 s", "partial", "hidden"]
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(20_480), "20.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
