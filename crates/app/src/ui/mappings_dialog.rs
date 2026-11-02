//! Linking fields to collections, so lookups find what the names don't say:
//! one field from its menu, or every link of a database, with copying from
//! another database.

use eframe::egui::{self, RichText, TextEdit};
use nordpil_backend::references::Mapping;
use uuid::Uuid;

use crate::widgets::kit::{self, Tone};
use crate::{icons, theme};

/// Another database's links, offered by "Copy from".
pub struct Source {
    /// E.g. `Production / shop`.
    pub label: String,
    pub mappings: Vec<Mapping>,
}

pub struct MappingsDialog {
    pub connection: Uuid,
    pub database: String,
    /// E.g. `Local dev / shop`.
    location: String,
    kind: Kind,
    error: Option<String>,
}

enum Kind {
    /// The link of fields named `key`; the rest of `mappings` stays as is.
    Link {
        key: String,
        row: Row,
        linked: bool,
        mappings: Vec<Mapping>,
    },
    /// Every link of the database.
    List {
        rows: Vec<Row>,
        sources: Vec<Source>,
    },
}

/// A link as edited: empty `field` means `_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Row {
    key: String,
    collection: String,
    field: String,
}

impl From<&Mapping> for Row {
    fn from(mapping: &Mapping) -> Self {
        Self {
            key: mapping.key.clone(),
            collection: mapping.collection.clone(),
            field: mapping.field.clone().unwrap_or_default(),
        }
    }
}

pub enum Outcome {
    Open,
    Cancel,
    /// The database's links after the edit.
    Save(Vec<Mapping>),
}

impl MappingsDialog {
    /// Links fields named `key`, or edits their link.
    pub fn link(
        connection: Uuid,
        database: String,
        location: String,
        key: String,
        mappings: Vec<Mapping>,
    ) -> Self {
        let existing = mappings.iter().find(|mapping| mapping.key == key);
        let row = existing.map_or_else(
            || Row {
                key: key.clone(),
                ..Row::default()
            },
            Row::from,
        );
        Self {
            connection,
            database,
            location,
            kind: Kind::Link {
                linked: existing.is_some(),
                key,
                row,
                mappings,
            },
            error: None,
        }
    }

    /// Edits every link of the database.
    pub fn list(
        connection: Uuid,
        database: String,
        location: String,
        mappings: &[Mapping],
        sources: Vec<Source>,
    ) -> Self {
        Self {
            connection,
            database,
            location,
            kind: Kind::List {
                rows: mappings.iter().map(Row::from).collect(),
                sources,
            },
            error: None,
        }
    }

    /// `collections` are the database's, for the pickers; empty until loaded.
    pub fn show(&mut self, ctx: &egui::Context, collections: &[String]) -> Outcome {
        let mut outcome = Outcome::Open;
        let list = matches!(self.kind, Kind::List { .. });
        let modal = kit::modal(ctx, egui::Id::unique(("mappings_dialog", list))).show(ctx, |ui| {
            match &mut self.kind {
                Kind::Link {
                    key,
                    row,
                    linked,
                    mappings,
                } => {
                    kit::dialog_width(ui, 480.0);
                    let title = if *linked {
                        "Edit link"
                    } else {
                        "Link to collection"
                    };
                    kit::dialog_title(ui, title, Some(&self.location));
                    ui.label(format!(
                        "Values of fields named {key} are found in this collection."
                    ));
                    ui.add_space(8.0);
                    link_form(ui, row, collections);
                    if let Some(error) = &self.error {
                        super::dialogs::error_label(ui, error);
                    }
                    kit::dialog_footer(ui, |ui| {
                        if kit::button(ui, Tone::Primary, None, "Save", kit::DIALOG_HEIGHT)
                            .clicked()
                        {
                            let mut rows: Vec<Row> = mappings.iter().map(Row::from).collect();
                            match rows.iter_mut().find(|other| other.key == *key) {
                                Some(existing) => *existing = row.clone(),
                                None => rows.push(row.clone()),
                            }
                            match to_mappings(&rows) {
                                Ok(saved) => outcome = Outcome::Save(saved),
                                Err(error) => self.error = Some(error),
                            }
                        }
                        if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT)
                            .clicked()
                        {
                            outcome = Outcome::Cancel;
                        }
                        if *linked
                            && kit::button(ui, Tone::Danger, None, "Remove", kit::DIALOG_HEIGHT)
                                .clicked()
                        {
                            let rest = mappings
                                .iter()
                                .filter(|mapping| mapping.key != *key)
                                .cloned()
                                .collect();
                            outcome = Outcome::Save(rest);
                        }
                    });
                }
                Kind::List { rows, sources } => {
                    kit::dialog_width(ui, 680.0);
                    kit::dialog_title(ui, "Reference mappings", Some(&self.location));
                    ui.label(
                        RichText::new(
                            "Right-clicking a field with a linked name offers to open the \
                             document it refers to.",
                        )
                        .color(theme::palette(ui).muted),
                    );
                    ui.add_space(8.0);
                    rows_table(ui, rows, collections);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if kit::button(ui, Tone::Ghost, Some(icons::PLUS), "Add", 32.0).clicked() {
                            rows.push(Row::default());
                        }
                        ui.add_enabled_ui(!sources.is_empty(), |ui| {
                            let copy =
                                kit::button(ui, Tone::Ghost, Some(icons::COPY), "Copy from…", 32.0);
                            egui::Popup::menu(&copy).show(|ui| {
                                for source in sources.iter() {
                                    if ui.button(&source.label).clicked() {
                                        merge(rows, &source.mappings);
                                    }
                                }
                            });
                        })
                        .response
                        .on_disabled_hover_text("No other database has links yet.");
                    });
                    if let Some(error) = &self.error {
                        super::dialogs::error_label(ui, error);
                    }
                    kit::dialog_footer(ui, |ui| {
                        if kit::button(ui, Tone::Primary, None, "Save", kit::DIALOG_HEIGHT)
                            .clicked()
                        {
                            match to_mappings(rows) {
                                Ok(saved) => outcome = Outcome::Save(saved),
                                Err(error) => self.error = Some(error),
                            }
                        }
                        if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT)
                            .clicked()
                        {
                            outcome = Outcome::Cancel;
                        }
                    });
                }
            }
        });
        if modal.should_close() {
            outcome = Outcome::Cancel;
        }
        outcome
    }
}

/// The collection and field of one link.
fn link_form(ui: &mut egui::Ui, row: &mut Row, collections: &[String]) {
    egui::Grid::new("link_form")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            let label = ui.label("Collection");
            collection_picker(
                ui,
                "link_collection",
                &mut row.collection,
                collections,
                300.0,
            )
            .labelled_by(label.id);
            ui.end_row();

            let label = ui.label("Field");
            ui.add(
                TextEdit::singleline(&mut row.field)
                    .hint_text("_id")
                    .desired_width(300.0),
            )
            .labelled_by(label.id);
            ui.end_row();
        });
    missing_warning(ui, row, collections);
}

fn rows_table(ui: &mut egui::Ui, rows: &mut Vec<Row>, collections: &[String]) {
    if rows.is_empty() {
        ui.weak("No links yet.");
        return;
    }
    let mut remove = None;
    egui::Grid::new("mapping_rows")
        .num_columns(5)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            let [key_heading, collection_heading, field_heading] =
                ["Field name", "Collection", "Field there"]
                    .map(|heading| kit::section_label(ui, heading).id);
            ui.label("");
            ui.label("");
            ui.end_row();
            for (index, row) in rows.iter_mut().enumerate() {
                // A grid cell is only as wide as its column was, so give the
                // fields their size outright.
                let height = ui.spacing().interact_size.y;
                ui.add_sized(
                    [170.0, height],
                    TextEdit::singleline(&mut row.key).hint_text("ownerRef"),
                )
                .labelled_by(key_heading);
                collection_picker(
                    ui,
                    ("row_collection", index),
                    &mut row.collection,
                    collections,
                    170.0,
                )
                .labelled_by(collection_heading);
                ui.add_sized(
                    [110.0, height],
                    TextEdit::singleline(&mut row.field).hint_text("_id"),
                )
                .labelled_by(field_heading);
                if kit::icon_button(ui, icons::TRASH, "Remove link", 28.0).clicked() {
                    remove = Some(index);
                }
                if is_missing(row, collections) {
                    ui.label(RichText::new(icons::TRIANGLE_ALERT).color(theme::palette(ui).warn))
                        .on_hover_text("Not in this database");
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        rows.remove(index);
    }
}

/// A combo box of the database's collections. A collection that isn't one
/// of them, e.g. copied from another database, stays chosen.
fn collection_picker(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    collection: &mut String,
    collections: &[String],
    width: f32,
) -> egui::Response {
    let selected = if collection.is_empty() {
        "Choose…"
    } else {
        collection.as_str()
    };
    egui::ComboBox::from_id_salt(id)
        .width(width)
        .selected_text(selected.to_owned())
        .show_ui(ui, |ui| {
            if collections.is_empty() {
                ui.weak("Loading collections…");
            }
            for name in collections {
                ui.selectable_value(collection, name.clone(), name.as_str());
            }
        })
        .response
}

fn is_missing(row: &Row, collections: &[String]) -> bool {
    !row.collection.is_empty() && !collections.is_empty() && !collections.contains(&row.collection)
}

fn missing_warning(ui: &mut egui::Ui, row: &Row, collections: &[String]) {
    if is_missing(row, collections) {
        ui.label(
            RichText::new(format!(
                "{}  {} is not in this database.",
                icons::TRIANGLE_ALERT,
                row.collection
            ))
            .color(theme::palette(ui).warn),
        );
    }
}

/// Adds `source`'s links; a key both have takes `source`'s collection.
fn merge(rows: &mut Vec<Row>, source: &[Mapping]) {
    for mapping in source {
        let copied = Row::from(mapping);
        match rows.iter_mut().find(|row| row.key.trim() == mapping.key) {
            Some(row) => *row = copied,
            None => rows.push(copied),
        }
    }
}

/// Checks the rows and turns them into links. Blank rows are skipped.
fn to_mappings(rows: &[Row]) -> Result<Vec<Mapping>, String> {
    let mut mappings: Vec<Mapping> = Vec::new();
    for row in rows {
        let (key, collection, field) = (row.key.trim(), row.collection.trim(), row.field.trim());
        if key.is_empty() && collection.is_empty() && field.is_empty() {
            continue;
        }
        if key.is_empty() {
            return Err("Each link needs a field name.".to_owned());
        }
        if collection.is_empty() {
            return Err(format!("Choose a collection for {key}."));
        }
        if mappings.iter().any(|mapping| mapping.key == key) {
            return Err(format!("{key} is linked twice."));
        }
        mappings.push(Mapping {
            key: key.to_owned(),
            collection: collection.to_owned(),
            field: match field {
                "" | "_id" => None,
                field => Some(field.to_owned()),
            },
        });
    }
    Ok(mappings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(key: &str, collection: &str) -> Mapping {
        Mapping {
            key: key.to_owned(),
            collection: collection.to_owned(),
            field: None,
        }
    }

    #[test]
    fn copy_from_merges_and_the_source_wins() {
        let mut rows: Vec<Row> = [mapping("ownerRef", "users"), mapping("tag", "tags")]
            .iter()
            .map(Row::from)
            .collect();

        merge(
            &mut rows,
            &[mapping("ownerRef", "accounts"), mapping("sku", "products")],
        );

        assert_eq!(
            to_mappings(&rows).unwrap(),
            [
                mapping("ownerRef", "accounts"),
                mapping("tag", "tags"),
                mapping("sku", "products"),
            ]
        );
    }

    #[test]
    fn saving_checks_rows() {
        let row = |key: &str, collection: &str, field: &str| Row {
            key: key.to_owned(),
            collection: collection.to_owned(),
            field: field.to_owned(),
        };

        assert_eq!(
            to_mappings(&[
                row(" a ", "x", " _id "),
                row("", "", ""),
                row("b", "y", "code")
            ]),
            Ok(vec![
                mapping("a", "x"),
                Mapping {
                    field: Some("code".to_owned()),
                    ..mapping("b", "y")
                },
            ])
        );
        assert_eq!(
            to_mappings(&[row("", "x", "")]),
            Err("Each link needs a field name.".to_owned())
        );
        assert_eq!(
            to_mappings(&[row("a", "", "")]),
            Err("Choose a collection for a.".to_owned())
        );
        assert_eq!(
            to_mappings(&[row("a", "x", ""), row("a", "y", "")]),
            Err("a is linked twice.".to_owned())
        );
    }
}
