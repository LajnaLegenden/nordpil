//! Small modal dialogs: password prompt, confirmations, create collection.

use eframe::egui::{self, Button, Key, RichText, TextEdit};
use nordpil_backend::Password;
use uuid::Uuid;

use super::Action;

pub enum Dialog {
    Password(PasswordPrompt),
    DeleteConnection { id: Uuid, name: String },
    Drop(DropDialog),
    CreateCollection(CreateCollectionDialog),
}

pub struct PasswordPrompt {
    pub id: Uuid,
    pub name: String,
    pub password: String,
    pub remember: bool,
    pub keyring_available: bool,
    pub error: Option<String>,
}

pub struct DropDialog {
    pub connection: Uuid,
    pub database: String,
    pub collection: Option<String>,
    pub typed: String,
}

pub struct CreateCollectionDialog {
    pub connection: Uuid,
    pub database: String,
    /// False when the dialog creates a new database.
    pub database_fixed: bool,
    pub collection: String,
}

impl Dialog {
    /// Returns false once the dialog is done.
    pub fn show(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
        let mut open = true;
        let modal = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            open = match self {
                Self::Password(prompt) => prompt.show(ui, actions),
                Self::DeleteConnection { id, name } => delete_connection(ui, *id, name, actions),
                Self::Drop(dialog) => dialog.show(ui, actions),
                Self::CreateCollection(dialog) => dialog.show(ui, actions),
            };
        });
        open && !modal.should_close()
    }
}

impl PasswordPrompt {
    fn show(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> bool {
        ui.heading(format!("Connect to {}", self.name));
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let label = ui.label("Password");
        let field = ui
            .add(
                TextEdit::singleline(&mut self.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(label.id);
        focus_once(ui, &field);
        if self.keyring_available {
            ui.checkbox(&mut self.remember, "Save password in the system keyring");
        }
        let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

        let mut open = true;
        ui.horizontal(|ui| {
            if ui.button("Connect").clicked() || submitted {
                actions.push(Action::ConnectWithPassword {
                    id: self.id,
                    password: Password::new(&self.password),
                    remember: self.remember && self.keyring_available,
                });
                open = false;
            }
            if ui.button("Cancel").clicked() {
                open = false;
            }
        });
        open
    }
}

fn delete_connection(ui: &mut egui::Ui, id: Uuid, name: &str, actions: &mut Vec<Action>) -> bool {
    ui.heading("Delete connection");
    ui.label(format!(
        "Delete the saved connection \"{name}\"? Its saved password is removed too. \
         No data on the server is touched."
    ));
    let mut open = true;
    ui.horizontal(|ui| {
        let delete = RichText::new("Delete").color(ui.visuals().error_fg_color);
        if ui.button(delete).clicked() {
            actions.push(Action::DeleteConnectionConfirmed(id));
            open = false;
        }
        if ui.button("Cancel").clicked() {
            open = false;
        }
    });
    open
}

impl DropDialog {
    fn target(&self) -> &str {
        self.collection.as_deref().unwrap_or(&self.database)
    }

    fn show(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> bool {
        match &self.collection {
            Some(collection) => {
                ui.heading("Drop collection");
                ui.label(format!(
                    "This permanently deletes {}.{collection} with all its documents and indexes.",
                    self.database
                ));
            }
            None => {
                ui.heading("Drop database");
                ui.label(format!(
                    "This permanently deletes the database {} with all its collections.",
                    self.database
                ));
            }
        }
        let label = ui.label(format!("Type \"{}\" to confirm:", self.target()));
        let field = ui
            .add(TextEdit::singleline(&mut self.typed).desired_width(f32::INFINITY))
            .labelled_by(label.id);
        focus_once(ui, &field);

        let confirmed = self.typed == self.target();
        let mut open = true;
        ui.horizontal(|ui| {
            let text = RichText::new("Drop").color(ui.visuals().error_fg_color);
            if ui.add_enabled(confirmed, Button::new(text)).clicked() {
                actions.push(Action::DropConfirmed {
                    connection: self.connection,
                    database: self.database.clone(),
                    collection: self.collection.clone(),
                });
                open = false;
            }
            if ui.button("Cancel").clicked() {
                open = false;
            }
        });
        open
    }
}

impl CreateCollectionDialog {
    fn show(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> bool {
        ui.heading(if self.database_fixed {
            "Create collection"
        } else {
            "Create database"
        });
        egui::Grid::new("create_collection")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let label = ui.label("Database");
                let field = ui
                    .add_enabled(
                        !self.database_fixed,
                        TextEdit::singleline(&mut self.database).desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                if !self.database_fixed {
                    focus_once(ui, &field);
                }
                ui.end_row();

                let label = ui.label("Collection");
                let field = ui
                    .add(TextEdit::singleline(&mut self.collection).desired_width(f32::INFINITY))
                    .labelled_by(label.id);
                if self.database_fixed {
                    focus_once(ui, &field);
                }
                ui.end_row();
            });
        if !self.database_fixed {
            ui.weak("A database is created together with its first collection.");
        }

        let valid = !self.database.trim().is_empty() && !self.collection.trim().is_empty();
        let mut open = true;
        ui.horizontal(|ui| {
            if ui.add_enabled(valid, Button::new("Create")).clicked() {
                actions.push(Action::CreateCollectionConfirmed {
                    connection: self.connection,
                    database: self.database.trim().to_owned(),
                    collection: self.collection.trim().to_owned(),
                });
                open = false;
            }
            if ui.button("Cancel").clicked() {
                open = false;
            }
        });
        open
    }
}

/// Focuses the first text field of a dialog when it opens.
fn focus_once(ui: &egui::Ui, field: &egui::Response) {
    if ui.memory(|m| m.focused().is_none()) {
        field.request_focus();
    }
}
