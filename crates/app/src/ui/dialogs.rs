//! Small modal dialogs: password prompt, confirmations, create collection.

use eframe::egui::{self, Key, RichText, TextEdit};
use nordpil_backend::Password;
use uuid::Uuid;

use super::Action;
use crate::theme;
use crate::widgets::kit::{self, Tone};

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
        // One id per kind, so a dialog never reuses another's size and position.
        let kind = match self {
            Self::Password(_) => "password",
            Self::DeleteConnection { .. } => "delete_connection",
            Self::Drop(_) => "drop",
            Self::CreateCollection(_) => "create_collection",
        };
        let modal = kit::modal(ctx, egui::Id::unique(("dialog", kind))).show(ctx, |ui| {
            ui.set_width(440.0);
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
        kit::dialog_title(ui, &format!("Connect to {}", self.name), None);
        if let Some(error) = &self.error {
            error_label(ui, error);
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
        kit::dialog_footer(ui, |ui| {
            if kit::button(ui, Tone::Primary, None, "Connect", kit::DIALOG_HEIGHT).clicked()
                || submitted
            {
                actions.push(Action::ConnectWithPassword {
                    id: self.id,
                    password: Password::new(&self.password),
                    remember: self.remember && self.keyring_available,
                });
                open = false;
            }
            if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                open = false;
            }
        });
        open
    }
}

fn delete_connection(ui: &mut egui::Ui, id: Uuid, name: &str, actions: &mut Vec<Action>) -> bool {
    kit::dialog_title(ui, "Delete connection", None);
    ui.label(format!(
        "Delete the saved connection \"{name}\"? Its saved password is removed too. \
         No data on the server is touched."
    ));
    let mut open = true;
    kit::dialog_footer(ui, |ui| {
        if kit::button(
            ui,
            Tone::Danger,
            Some(crate::icons::TRASH),
            "Delete",
            kit::DIALOG_HEIGHT,
        )
        .clicked()
        {
            actions.push(Action::DeleteConnectionConfirmed(id));
            open = false;
        }
        if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
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
                kit::dialog_title(ui, "Drop collection", None);
                ui.label(format!(
                    "This permanently deletes {}.{collection} with all its documents and indexes.",
                    self.database
                ));
            }
            None => {
                kit::dialog_title(ui, "Drop database", None);
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
        kit::dialog_footer(ui, |ui| {
            let drop = ui
                .add_enabled_ui(confirmed, |ui| {
                    kit::button(
                        ui,
                        Tone::Danger,
                        Some(crate::icons::TRASH),
                        "Drop",
                        kit::DIALOG_HEIGHT,
                    )
                })
                .inner;
            if drop.clicked() {
                actions.push(Action::DropConfirmed {
                    connection: self.connection,
                    database: self.database.clone(),
                    collection: self.collection.clone(),
                });
                open = false;
            }
            if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                open = false;
            }
        });
        open
    }
}

impl CreateCollectionDialog {
    fn show(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> bool {
        kit::dialog_title(
            ui,
            if self.database_fixed {
                "Create collection"
            } else {
                "Create database"
            },
            None,
        );
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
        kit::dialog_footer(ui, |ui| {
            let create = ui
                .add_enabled_ui(valid, |ui| {
                    kit::button(ui, Tone::Primary, None, "Create", kit::DIALOG_HEIGHT)
                })
                .inner;
            if create.clicked() {
                actions.push(Action::CreateCollectionConfirmed {
                    connection: self.connection,
                    database: self.database.trim().to_owned(),
                    collection: self.collection.trim().to_owned(),
                });
                open = false;
            }
            if kit::button(ui, Tone::Secondary, None, "Cancel", kit::DIALOG_HEIGHT).clicked() {
                open = false;
            }
        });
        open
    }
}

pub fn error_label(ui: &mut egui::Ui, error: &str) {
    let palette = theme::palette(ui);
    ui.label(
        RichText::new(format!("{}  {error}", crate::icons::CIRCLE_ALERT)).color(palette.danger),
    );
}

/// Focuses the first text field of a dialog when it opens.
fn focus_once(ui: &egui::Ui, field: &egui::Response) {
    if ui.memory(|m| m.focused().is_none()) {
        field.request_focus();
    }
}
