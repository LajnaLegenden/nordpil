//! A collection's indexes: keys, properties, size and usage, with actions.

use bson::{Bson, Document};
use eframe::egui::{self, Align, Layout, Margin, RichText, Stroke};
use egui_extras::{Column, TableBuilder};
use nordpil_backend::IndexInfo;
use nordpil_query::{Highlight, Style, format_document, format_value};

use crate::icons;
use crate::tabs::CollectionTab;
use crate::theme;
use crate::widgets::code_editor;
use crate::widgets::kit::{self, Tone};

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
    let palette = theme::palette(ui);
    let mut action = None;
    ui.horizontal(|ui| {
        if !read_only
            && kit::button(ui, Tone::Primary, Some(icons::PLUS), "Create index", 36.0).clicked()
        {
            action = Some(IndexAction::Create);
        }
        if tab.indexes_request.is_some() {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if kit::icon_button(ui, icons::REFRESH, "Refresh", kit::TOOLBAR_HEIGHT).clicked() {
                action = Some(IndexAction::Refresh);
            }
            if let Some(Ok(indexes)) = &tab.indexes {
                let total: u64 = indexes.iter().filter_map(|index| index.size).sum();
                let count = match indexes.len() {
                    1 => "1 index".to_owned(),
                    n => format!("{n} indexes"),
                };
                ui.label(
                    RichText::new(format!("{count} · {}", human_bytes(total)))
                        .size(13.0)
                        .color(palette.subtle),
                );
            }
        });
    });
    ui.add_space(12.0);

    let indexes = match &tab.indexes {
        None => return action,
        Some(Err(error)) => {
            ui.label(
                RichText::new(format!("{}  {error}", icons::CIRCLE_ALERT)).color(palette.danger),
            );
            return action;
        }
        Some(Ok(indexes)) => indexes,
    };
    let key_color = code_editor::color(Highlight::Key, ui.visuals());
    let number_color = code_editor::color(Highlight::Number, ui.visuals());
    kit::card(ui).inner_margin(Margin::ZERO).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            kit::table_header_band(ui, 40.0);
            TableBuilder::new(ui)
                .id_salt(("indexes", tab.id))
                .striped(false)
                .resizable(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(200.0).at_least(80.0).clip(true))
                .column(Column::initial(250.0).at_least(80.0).clip(true))
                .column(Column::initial(190.0).at_least(80.0).clip(true))
                .column(Column::initial(90.0).at_least(50.0))
                .column(Column::initial(100.0).at_least(60.0).clip(true))
                .column(Column::remainder().at_least(170.0))
                .header(40.0, |mut header| {
                    for name in ["Name", "Keys", "Properties", "Size", "Usage", ""] {
                        header.col(|ui| {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(name)
                                    .family(theme::semibold())
                                    .size(13.0)
                                    .color(palette.text),
                            );
                        });
                    }
                })
                .body(|body| {
                    body.rows(48.0, indexes.len(), |mut row| {
                        let index = &indexes[row.index()];
                        let hidden = index.spec.get_bool("hidden").unwrap_or(false);
                        let cell = |row: &mut egui_extras::TableRow<'_, '_>,
                                        add: &mut dyn FnMut(&mut egui::Ui)| {
                            row.col(|ui| {
                                if hidden {
                                    ui.multiply_opacity(0.62);
                                }
                                ui.add_space(10.0);
                                add(ui);
                                let rect = ui.max_rect();
                                ui.painter().hline(
                                    rect.x_range(),
                                    rect.bottom(),
                                    Stroke::new(1.0, palette.line),
                                );
                            });
                        };
                        cell(&mut row, &mut |ui| {
                            ui.label(RichText::new(icons::KEY).size(15.0).color(palette.subtle));
                            ui.label(RichText::new(&index.name).family(theme::medium()));
                        });
                        cell(&mut row, &mut |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let keys = index.keys();
                            ui.label(RichText::new("{ ").monospace().size(13.0).color(palette.subtle));
                            for (position, (key, value)) in keys.iter().enumerate() {
                                if position > 0 {
                                    ui.label(
                                        RichText::new(", ").monospace().size(13.0).color(palette.subtle),
                                    );
                                }
                                let key = format_document(&bson::doc! { key: 1 }, Style::Compact);
                                let key = key
                                    .trim_start_matches("{ ")
                                    .trim_end_matches(": 1 }")
                                    .to_owned();
                                ui.label(RichText::new(key).monospace().size(13.0).color(key_color));
                                ui.label(RichText::new(": ").monospace().size(13.0).color(palette.subtle));
                                ui.label(
                                    RichText::new(format_value(value, Style::Compact))
                                        .monospace()
                                        .size(13.0)
                                        .color(number_color),
                                );
                            }
                            ui.label(RichText::new(" }").monospace().size(13.0).color(palette.subtle));
                        });
                        cell(&mut row, &mut |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let spec = format_document(&index.spec, Style::Pretty(2));
                            let properties = properties(index);
                            if properties.is_empty() {
                                ui.label(RichText::new("–").color(palette.subtle));
                            }
                            for property in properties {
                                kit::pill(ui, &property, property.starts_with("TTL"))
                                    .on_hover_text(&spec);
                            }
                        });
                        cell(&mut row, &mut |ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new(index.size.map_or_else(|| "–".to_owned(), human_bytes))
                                        .color(palette.muted),
                                );
                            });
                        });
                        cell(&mut row, &mut |ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(10.0);
                                let label = ui.label(
                                    RichText::new(index.ops.map_or_else(
                                        || "–".to_owned(),
                                        |ops| format!("{} uses", crate::ui::documents::thousands(u64::try_from(ops).unwrap_or(0))),
                                    ))
                                    .color(palette.muted),
                                );
                                if let Some(since) = index.since {
                                    label.on_hover_text(format!(
                                        "Since {}",
                                        format_value(&Bson::DateTime(since), Style::Compact)
                                    ));
                                }
                            });
                        });
                        row.col(|ui| {
                            let rect = ui.max_rect();
                            ui.painter().hline(
                                rect.x_range(),
                                rect.bottom(),
                                Stroke::new(1.0, palette.line),
                            );
                            if read_only || index.is_id_index() {
                                return;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(8.0);
                                if kit::button(ui, Tone::Ghost, Some(icons::TRASH), "Drop", 30.0)
                                    .clicked()
                                {
                                    action = Some(IndexAction::Drop(index.name.clone()));
                                }
                                let (toggle, icon) = if hidden {
                                    ("Unhide", icons::EYE)
                                } else {
                                    ("Hide", icons::EYE_OFF)
                                };
                                if kit::button(ui, Tone::Ghost, Some(icon), toggle, 30.0)
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
                            });
                        });
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
