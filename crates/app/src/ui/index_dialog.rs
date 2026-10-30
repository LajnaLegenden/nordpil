//! Creating an index, and confirming a drop.

use bson::{Bson, Document};
use eframe::egui::{self, Button, RichText, TextEdit};
use nordpil_backend::{Command, Namespace, RequestId};
use nordpil_query::{FieldHint, Mode};

use crate::query_language::QueryLanguage;
use crate::widgets::code_editor::CodeEditor;

pub enum Kind {
    Create(Box<CreateForm>),
    Drop { name: String, typed: String },
}

pub struct CreateForm {
    keys: String,
    keys_editor: CodeEditor,
    name: String,
    unique: bool,
    sparse: bool,
    hidden: bool,
    ttl_seconds: String,
    partial_filter: String,
    partial_editor: CodeEditor,
    collation: String,
    collation_editor: CodeEditor,
}

pub struct IndexDialog {
    pub namespace: Namespace,
    pub kind: Kind,
    pub pending: Option<RequestId>,
    pub error: Option<String>,
}

pub enum Outcome {
    Open,
    Cancel,
    Send(Box<Command>),
}

impl IndexDialog {
    pub fn create(namespace: Namespace) -> Self {
        let editor = |name: &str, hint: &str| {
            CodeEditor::new(egui::Id::unique(("index_dialog", name)), false).hint(hint)
        };
        Self {
            namespace,
            kind: Kind::Create(Box::new(CreateForm {
                keys: String::new(),
                keys_editor: editor("keys", "{ field: 1, other: -1 }"),
                name: String::new(),
                unique: false,
                sparse: false,
                hidden: false,
                ttl_seconds: String::new(),
                partial_filter: String::new(),
                partial_editor: editor("partial", "{ field: { $exists: true } }"),
                collation: String::new(),
                collation_editor: editor("collation", "{ locale: 'en', strength: 2 }"),
            })),
            pending: None,
            error: None,
        }
    }

    pub fn drop(namespace: Namespace, name: String) -> Self {
        Self {
            namespace,
            kind: Kind::Drop {
                name,
                typed: String::new(),
            },
            pending: None,
            error: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, fields: &[FieldHint]) -> Outcome {
        let mut outcome = Outcome::Open;
        let busy = self.pending.is_some();
        // One id per kind, so a dialog never reuses another's size and position.
        let kind = matches!(self.kind, Kind::Create(_));
        let modal = egui::Modal::new(egui::Id::unique(("index_dialog", kind))).show(ctx, |ui| {
            ui.set_width(520.0);
            let (confirm, enabled) = match &mut self.kind {
                Kind::Create(form) => {
                    ui.heading("Create index");
                    ui.add_enabled_ui(!busy, |ui| form.show(ui, fields));
                    ("Create", true)
                }
                Kind::Drop { name, typed } => {
                    ui.heading("Drop index");
                    ui.label(format!(
                        "Queries that use {name} will get slower. Rebuilding it later can take a while."
                    ));
                    let label = ui.label(format!("Type \"{name}\" to confirm:"));
                    ui.add(TextEdit::singleline(typed).desired_width(f32::INFINITY))
                        .labelled_by(label.id);
                    let confirmed = *typed == *name;
                    ("Drop", confirmed)
                }
            };
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let mut text = RichText::new(confirm);
                if matches!(self.kind, Kind::Drop { .. }) {
                    text = text.color(ui.visuals().error_fg_color);
                }
                if ui.add_enabled(enabled && !busy, Button::new(text)).clicked() {
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
        if modal.should_close() && !busy {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    fn command(&self) -> Result<Command, String> {
        let namespace = self.namespace.clone();
        match &self.kind {
            Kind::Create(form) => {
                let (keys, options) = form.build()?;
                Ok(Command::CreateIndex {
                    namespace,
                    keys,
                    options,
                })
            }
            Kind::Drop { name, .. } => Ok(Command::DropIndex {
                namespace,
                name: name.clone(),
            }),
        }
    }
}

impl CreateForm {
    fn show(&mut self, ui: &mut egui::Ui, fields: &[FieldHint]) {
        egui::Grid::new("create_index")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let label = ui.label("Keys");
                let language = QueryLanguage::new(Mode::Fields, fields);
                self.keys_editor
                    .show(ui, &mut self.keys, &language)
                    .response
                    .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("Name");
                ui.add(
                    TextEdit::singleline(&mut self.name)
                        .hint_text("made from the keys")
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id);
                ui.end_row();

                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.unique, "Unique");
                    ui.checkbox(&mut self.sparse, "Sparse");
                    ui.checkbox(&mut self.hidden, "Hidden");
                });
                ui.end_row();

                let label = ui.label("TTL (seconds)");
                ui.add(
                    TextEdit::singleline(&mut self.ttl_seconds)
                        .hint_text("none")
                        .desired_width(100.0),
                )
                .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("Partial filter");
                let language = QueryLanguage::new(Mode::Filter, fields);
                self.partial_editor
                    .show(ui, &mut self.partial_filter, &language)
                    .response
                    .labelled_by(label.id);
                ui.end_row();

                let label = ui.label("Collation");
                let language = QueryLanguage::new(Mode::Collation, fields);
                self.collation_editor
                    .show(ui, &mut self.collation, &language)
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            });
        ui.weak("Key values: 1 or -1 for ascending/descending, or 'text', '2dsphere', 'hashed'.");
    }

    fn build(&self) -> Result<(Document, Document), String> {
        let document = |name: &str, text: &str| {
            nordpil_query::parse_document(text).map_err(|errors| {
                let message = errors.first().map_or("", |e| e.message.as_str());
                format!("{name}: {message}")
            })
        };
        let keys = document("Keys", &self.keys)?;
        if keys.is_empty() {
            return Err("Enter at least one key, e.g. { field: 1 }.".to_owned());
        }
        let mut options = Document::new();
        let name = self.name.trim();
        if !name.is_empty() {
            options.insert("name", name);
        }
        for (flag, set) in [
            ("unique", self.unique),
            ("sparse", self.sparse),
            ("hidden", self.hidden),
        ] {
            if set {
                options.insert(flag, true);
            }
        }
        let ttl = self.ttl_seconds.trim();
        if !ttl.is_empty() {
            let seconds: i64 = ttl
                .parse()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or("TTL must be a whole number of seconds.")?;
            options.insert("expireAfterSeconds", Bson::Int64(seconds));
        }
        let partial = document("Partial filter", &self.partial_filter)?;
        if !partial.is_empty() {
            options.insert("partialFilterExpression", partial);
        }
        let collation = document("Collation", &self.collation)?;
        if !collation.is_empty() {
            options.insert("collation", collation);
        }
        Ok((keys, options))
    }
}
