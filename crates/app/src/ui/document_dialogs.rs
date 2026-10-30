//! Dialogs that write documents: insert (and clone), edit, delete, and bulk
//! update or delete of everything matching the filter.

use bson::{Bson, Document};
use eframe::egui::{self, Button, RichText};
use nordpil_backend::{Command, Namespace, RequestId};
use nordpil_query::{FieldHint, Mode, Style, format_document, format_value};

use crate::query_language::QueryLanguage;
use crate::widgets::code_editor::CodeEditor;

pub enum Kind {
    Insert,
    Edit { original: Box<Document> },
    Delete { id: Bson },
    UpdateMany { filter: Document },
    DeleteMany { filter: Document },
}

pub struct DocumentDialog {
    pub namespace: Namespace,
    pub kind: Kind,
    text: String,
    editor: CodeEditor,
    /// The write being saved; the dialog closes when it succeeds.
    pub pending: Option<RequestId>,
    /// The count of matching documents, for bulk dialogs.
    pub count_request: Option<RequestId>,
    /// `None` while counting, `Some(None)` when there were too many to count.
    pub count: Option<Option<u64>>,
    pub error: Option<String>,
}

pub enum Outcome {
    Open,
    Cancel,
    Send(Box<Command>),
}

impl DocumentDialog {
    pub fn new(namespace: Namespace, kind: Kind) -> Self {
        let text = match &kind {
            Kind::Insert => "{\n  \n}".to_owned(),
            Kind::Edit { original } => format_document(original, Style::Pretty(2)),
            Kind::UpdateMany { .. } => "{ $set: {  } }".to_owned(),
            Kind::Delete { .. } | Kind::DeleteMany { .. } => String::new(),
        };
        let multiline = !matches!(kind, Kind::UpdateMany { .. });
        Self {
            editor: CodeEditor::new(egui::Id::unique("document_dialog"), multiline).rows(18),
            namespace,
            kind,
            text,
            pending: None,
            count_request: None,
            count: None,
            error: None,
        }
    }

    /// A new document prefilled with a copy of `document` minus its `_id`.
    pub fn clone_of(namespace: Namespace, document: &Document) -> Self {
        let mut copy = document.clone();
        copy.remove("_id");
        let mut dialog = Self::new(namespace, Kind::Insert);
        dialog.text = format_document(&copy, Style::Pretty(2));
        dialog
    }

    pub fn show(&mut self, ctx: &egui::Context, fields: &[FieldHint]) -> Outcome {
        let mut outcome = Outcome::Open;
        // One id per kind, so a dialog never reuses another's size and position.
        let modal_id = egui::Id::unique(("document_dialog", self.title()));
        let modal = egui::Modal::new(modal_id).show(ctx, |ui| {
            ui.set_width(640.0);
            let busy = self.pending.is_some();
            ui.heading(self.title());
            ui.weak(format!(
                "{}.{}",
                self.namespace.database, self.namespace.collection
            ));
            ui.add_space(6.0);

            let submitted = ui.add_enabled_ui(!busy, |ui| self.body(ui, fields)).inner;
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let (label, enabled) = self.confirm_button();
                let mut text = RichText::new(label);
                if self.is_destructive() {
                    text = text.color(ui.visuals().error_fg_color);
                }
                if ui
                    .add_enabled(enabled && !busy, Button::new(text))
                    .clicked()
                    || submitted
                {
                    match self.command() {
                        Ok(command) => {
                            self.error = None;
                            outcome = Outcome::Send(Box::new(command));
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                if ui.add_enabled(!busy, Button::new("Cancel")).clicked() {
                    outcome = Outcome::Cancel;
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        if modal.should_close() && self.pending.is_none() {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    fn title(&self) -> &'static str {
        match self.kind {
            Kind::Insert => "Insert documents",
            Kind::Edit { .. } => "Edit document",
            Kind::Delete { .. } => "Delete document",
            Kind::UpdateMany { .. } => "Update matching documents",
            Kind::DeleteMany { .. } => "Delete matching documents",
        }
    }

    fn is_destructive(&self) -> bool {
        matches!(self.kind, Kind::Delete { .. } | Kind::DeleteMany { .. })
    }

    /// Returns true when the editor asked to submit (Ctrl/Cmd+Enter).
    fn body(&mut self, ui: &mut egui::Ui, fields: &[FieldHint]) -> bool {
        match &self.kind {
            Kind::Insert => {
                ui.label("One document, or an array of documents. Ctrl+Enter inserts.");
                self.editor_field(ui, "Documents", fields)
            }
            Kind::Edit { .. } => {
                ui.label("Only changed fields are saved. Ctrl+Enter saves.");
                self.editor_field(ui, "Document", fields)
            }
            Kind::Delete { id } => {
                ui.label(format!(
                    "Delete the document with _id {}? This can't be undone.",
                    format_value(id, Style::Compact)
                ));
                false
            }
            Kind::UpdateMany { filter } | Kind::DeleteMany { filter } => {
                let filter_text = format_document(filter, Style::Compact);
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    ui.monospace(&filter_text);
                });
                ui.label(match self.count {
                    None => "Counting matching documents…".to_owned(),
                    Some(None) => "The number of matching documents is unknown.".to_owned(),
                    Some(Some(n)) => format!("{n} matching documents."),
                });
                if filter.is_empty() {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "The filter is empty, so this affects every document in the collection.",
                    );
                }
                if matches!(self.kind, Kind::UpdateMany { .. }) {
                    ui.add_space(4.0);
                    self.editor_field(ui, "Update", fields)
                } else {
                    false
                }
            }
        }
    }

    fn editor_field(&mut self, ui: &mut egui::Ui, label: &str, fields: &[FieldHint]) -> bool {
        let label = ui.label(label);
        let language = QueryLanguage::new(Mode::Document, fields);
        let output = egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| self.editor.show(ui, &mut self.text, &language))
            .inner;
        output.response.labelled_by(label.id);
        output.submitted
    }

    fn confirm_button(&self) -> (String, bool) {
        let count_text = |verb: &str| match self.count {
            Some(Some(n)) => (format!("{verb} {n} documents"), true),
            Some(None) => (format!("{verb} all matching"), true),
            None => (format!("{verb} all matching"), false),
        };
        match self.kind {
            Kind::Insert => ("Insert".to_owned(), true),
            Kind::Edit { .. } => ("Save".to_owned(), true),
            Kind::Delete { .. } => ("Delete".to_owned(), true),
            Kind::UpdateMany { .. } => count_text("Update"),
            Kind::DeleteMany { .. } => count_text("Delete"),
        }
    }

    fn command(&self) -> Result<Command, String> {
        let namespace = self.namespace.clone();
        Ok(match &self.kind {
            Kind::Insert => Command::InsertDocuments {
                namespace,
                documents: parse_documents(&self.text)?,
            },
            Kind::Edit { original } => Command::UpdateDocument {
                namespace,
                original: original.clone(),
                updated: Box::new(
                    nordpil_query::parse_document(&self.text)
                        .map_err(|errors| describe(&self.text, errors))?,
                ),
            },
            Kind::Delete { id } => Command::DeleteDocument {
                namespace,
                id: id.clone(),
            },
            Kind::UpdateMany { filter } => Command::UpdateMany {
                namespace,
                filter: filter.clone(),
                update: nordpil_query::parse_value(&self.text)
                    .map_err(|errors| describe(&self.text, errors))?,
            },
            Kind::DeleteMany { filter } => Command::DeleteMany {
                namespace,
                filter: filter.clone(),
            },
        })
    }
}

/// One document or an array of documents.
fn parse_documents(text: &str) -> Result<Vec<Document>, String> {
    match nordpil_query::parse_value(text).map_err(|errors| describe(text, errors))? {
        Bson::Document(document) => Ok(vec![document]),
        Bson::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Bson::Document(document) => Ok(document),
                _ => Err("Every item in the array must be a document.".to_owned()),
            })
            .collect(),
        _ => Err("Enter a document in braces, or an array of documents.".to_owned()),
    }
}

/// The first parse error, with its line and column.
fn describe(text: &str, errors: Vec<nordpil_query::ParseError>) -> String {
    let Some(error) = errors.first() else {
        return String::new();
    };
    let before = &text[..error.span.start.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("Line {line}, column {column}: {}", error.message)
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn inserts_one_document_or_an_array() {
        assert_eq!(parse_documents("{ a: 1 }"), Ok(vec![doc! { "a": 1 }]));
        assert_eq!(
            parse_documents("[{ a: 1 }, { a: 2 }]"),
            Ok(vec![doc! { "a": 1 }, doc! { "a": 2 }])
        );
        assert!(parse_documents("[1]").is_err());
    }

    #[test]
    fn edit_round_trips_types_through_text() {
        let original = doc! { "_id": 1, "n": 5_i64, "d": 1.0, "i": 2 };
        let dialog = DocumentDialog::new(
            Namespace {
                connection: uuid::Uuid::nil(),
                database: "db".to_owned(),
                collection: "c".to_owned(),
            },
            Kind::Edit {
                original: Box::new(original.clone()),
            },
        );
        let Ok(Command::UpdateDocument { updated, .. }) = dialog.command() else {
            panic!("expected an update");
        };
        assert_eq!(*updated, original);
    }
}
