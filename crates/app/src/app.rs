use eframe::egui;
use nordpil_backend::store::SavedConnection;
use nordpil_backend::{
    Backend, BackendError, Bridge, Command, DatabaseInfo, ErrorKind, Outcome, Password, RequestId,
    Response,
};
use uuid::Uuid;

use crate::connections::{Collections, ConnectionEntry, ConnectionState, Selection};
use crate::spike_language::SpikeLanguage;
use crate::ui::connection_form::{ConnectionForm, FormResult};
use crate::ui::dialogs::{CreateCollectionDialog, Dialog, DropDialog, PasswordPrompt};
use crate::ui::{Action, sidebar};
use crate::widgets::code_editor::CodeEditor;

/// Older notifications are dropped beyond this many.
const MAX_NOTIFICATIONS: usize = 5;

pub struct NordpilApp {
    backend: Bridge<Backend>,
    connections: Vec<ConnectionEntry>,
    keyring_available: bool,
    selection: Option<Selection>,
    form: Option<ConnectionForm>,
    /// Connect once the form's save succeeds.
    connect_after_save: Option<(Uuid, Option<Password>)>,
    dialog: Option<Dialog>,
    notifications: Vec<BackendError>,
    filter: String,
    filter_editor: CodeEditor,
    submitted_filter: Option<String>,
}

impl NordpilApp {
    /// `ctx` is repainted whenever a backend result arrives.
    pub fn new(ctx: &egui::Context, backend: Backend) -> std::io::Result<Self> {
        let ctx = ctx.clone();
        let mut backend = Bridge::new(backend, move || ctx.request_repaint())?;
        backend.send(Command::LoadConnections);

        Ok(Self {
            backend,
            connections: Vec::new(),
            keyring_available: false,
            selection: None,
            form: None,
            connect_after_save: None,
            dialog: None,
            notifications: Vec::new(),
            filter: String::new(),
            filter_editor: CodeEditor::new(egui::Id::unique("filter"), false)
                .hint("{ field: 'value' }"),
            submitted_filter: None,
        })
    }

    fn entry(&self, id: Uuid) -> Option<&ConnectionEntry> {
        self.connections.iter().find(|entry| entry.saved.id == id)
    }

    fn entry_mut(&mut self, id: Uuid) -> Option<&mut ConnectionEntry> {
        self.connections
            .iter_mut()
            .find(|entry| entry.saved.id == id)
    }

    fn notify(&mut self, error: BackendError) {
        tracing::warn!(message = %error.message, detail = ?error.detail, "showing error");
        self.notifications.push(error);
        if self.notifications.len() > MAX_NOTIFICATIONS {
            self.notifications.remove(0);
        }
    }

    fn connect(&mut self, id: Uuid, password: Option<Password>, remember_password: bool) {
        let request = self.backend.send(Command::Connect {
            id,
            password,
            remember_password,
        });
        if let Some(entry) = self.entry_mut(id) {
            entry.state = ConnectionState::Connecting(request);
        }
    }

    fn deselect_if(&mut self, matches: impl Fn(&Selection) -> bool) {
        if self.selection.as_ref().is_some_and(matches) {
            self.selection = None;
        }
    }

    fn handle_response(&mut self, response: Response) {
        match response {
            Response::Connections {
                result,
                keyring_available,
            } => {
                self.keyring_available = keyring_available;
                match result {
                    Ok(saved) => {
                        self.connections = saved.into_iter().map(ConnectionEntry::new).collect();
                    }
                    Err(error) => self.notify(error),
                }
            }
            Response::ConnectionSaved(result) => self.connection_saved(result),
            Response::ConnectionDeleted { id, result } => match result {
                Ok(()) => {
                    self.connections.retain(|entry| entry.saved.id != id);
                    self.deselect_if(|s| s.connection == id);
                }
                Err(error) => self.notify(error),
            },
            Response::Connected { id, result } => self.connected(id, result),
            Response::Disconnected(id) => {
                if let Some(entry) = self.entry_mut(id) {
                    entry.state = ConnectionState::Disconnected;
                }
                self.deselect_if(|s| s.connection == id);
            }
            Response::Databases { connection, result } => match result {
                Ok(databases) => {
                    if let Some(entry) = self.entry_mut(connection) {
                        entry.set_databases(databases);
                    }
                    self.deselect_missing_database(connection);
                }
                Err(error) => self.notify(error),
            },
            Response::Collections {
                connection,
                database,
                result,
            } => self.collections_loaded(connection, &database, result),
            Response::CollectionCreated {
                connection,
                database,
                result,
            }
            | Response::CollectionDropped {
                connection,
                database,
                result,
            } => match result {
                Ok(()) => {
                    self.apply(Action::RefreshDatabases(connection));
                    self.apply(Action::LoadCollections {
                        connection,
                        database,
                    });
                }
                Err(error) => self.notify(error),
            },
            Response::DatabaseDropped {
                connection,
                database,
                result,
            } => match result {
                Ok(()) => {
                    self.deselect_if(|s| s.connection == connection && s.database == database);
                    self.apply(Action::RefreshDatabases(connection));
                }
                Err(error) => self.notify(error),
            },
        }
    }

    fn connection_saved(&mut self, result: Result<SavedConnection, BackendError>) {
        let Some(form) = &mut self.form else {
            return;
        };
        match result {
            Ok(saved) => {
                self.form = None;
                match self.entry_mut(saved.id) {
                    Some(entry) => entry.saved = saved,
                    None => self.connections.push(ConnectionEntry::new(saved)),
                }
                if let Some((id, password)) = self.connect_after_save.take() {
                    self.connect(id, password, false);
                }
            }
            Err(error) => {
                form.saving = false;
                form.error = Some(error.message);
                self.connect_after_save = None;
            }
        }
    }

    fn connected(&mut self, id: Uuid, result: Result<Vec<DatabaseInfo>, BackendError>) {
        let keyring_available = self.keyring_available;
        let Some(entry) = self.entry_mut(id) else {
            return;
        };
        match result {
            Ok(databases) => entry.set_databases(databases),
            Err(error)
                if matches!(
                    error.kind,
                    ErrorKind::PasswordRequired | ErrorKind::AuthenticationFailed
                ) =>
            {
                entry.state = ConnectionState::Disconnected;
                self.dialog = Some(Dialog::Password(PasswordPrompt {
                    id,
                    name: entry.saved.name.clone(),
                    password: String::new(),
                    remember: keyring_available,
                    keyring_available,
                    error: (error.kind == ErrorKind::AuthenticationFailed).then_some(error.message),
                }));
            }
            Err(error) => {
                entry.state = ConnectionState::Failed(error.clone());
                self.notify(error);
            }
        }
    }

    fn collections_loaded(
        &mut self,
        connection: Uuid,
        database: &str,
        result: Result<Vec<nordpil_backend::CollectionInfo>, BackendError>,
    ) {
        let Some(node) = self
            .entry_mut(connection)
            .and_then(|entry| entry.database_mut(database))
        else {
            return;
        };
        let names: Vec<_> = result
            .iter()
            .flatten()
            .map(|collection| collection.name.clone())
            .collect();
        let loaded = result.is_ok();
        node.collections = match result {
            Ok(collections) => Collections::Loaded(collections),
            Err(error) => Collections::Failed(error),
        };
        if loaded {
            self.deselect_if(|s| {
                s.connection == connection
                    && s.database == database
                    && !names.contains(&s.collection)
            });
        }
    }

    fn deselect_missing_database(&mut self, connection: Uuid) {
        let Some(ConnectionState::Connected(databases)) =
            self.entry(connection).map(|entry| &entry.state)
        else {
            return;
        };
        let names: Vec<_> = databases.iter().map(|db| db.info.name.clone()).collect();
        self.deselect_if(|s| s.connection == connection && !names.contains(&s.database));
    }

    fn cancelled(&mut self, request: RequestId) {
        for entry in &mut self.connections {
            if matches!(entry.state, ConnectionState::Connecting(r) if r == request) {
                entry.state = ConnectionState::Disconnected;
            }
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::NewConnection => self.form = Some(ConnectionForm::new()),
            Action::EditConnection(id) => {
                self.form = self
                    .entry(id)
                    .map(|entry| ConnectionForm::edit(&entry.saved));
            }
            Action::DeleteConnection(id) => {
                if let Some(entry) = self.entry(id) {
                    let name = entry.saved.name.clone();
                    self.dialog = Some(Dialog::DeleteConnection { id, name });
                }
            }
            Action::DeleteConnectionConfirmed(id) => {
                self.backend.send(Command::DeleteConnection(id));
            }
            Action::Connect(id) => self.connect(id, None, false),
            Action::ConnectWithPassword {
                id,
                password,
                remember,
            } => self.connect(id, Some(password), remember),
            Action::CancelConnect(id) => {
                if let Some(ConnectionState::Connecting(request)) =
                    self.entry(id).map(|entry| &entry.state)
                {
                    self.backend.cancel(*request);
                }
            }
            Action::Disconnect(id) => {
                self.backend.send(Command::Disconnect(id));
            }
            Action::RefreshDatabases(id) => {
                self.backend.send(Command::ListDatabases(id));
            }
            Action::LoadCollections {
                connection,
                database,
            } => {
                if let Some(node) = self
                    .entry_mut(connection)
                    .and_then(|entry| entry.database_mut(&database))
                {
                    node.collections = Collections::Loading;
                }
                self.backend.send(Command::ListCollections {
                    connection,
                    database,
                });
            }
            Action::Select(selection) => self.selection = Some(selection),
            Action::CreateCollection {
                connection,
                database,
            } => {
                self.dialog = Some(Dialog::CreateCollection(CreateCollectionDialog {
                    connection,
                    database_fixed: database.is_some(),
                    database: database.unwrap_or_default(),
                    collection: String::new(),
                }));
            }
            Action::CreateCollectionConfirmed {
                connection,
                database,
                collection,
            } => {
                self.backend.send(Command::CreateCollection {
                    connection,
                    database,
                    collection,
                });
            }
            Action::Drop {
                connection,
                database,
                collection,
            } => {
                self.dialog = Some(Dialog::Drop(DropDialog {
                    connection,
                    database,
                    collection,
                    typed: String::new(),
                }));
            }
            Action::DropConfirmed {
                connection,
                database,
                collection: Some(collection),
            } => {
                self.backend.send(Command::DropCollection {
                    connection,
                    database,
                    collection,
                });
            }
            Action::DropConfirmed {
                connection,
                database,
                collection: None,
            } => {
                self.backend.send(Command::DropDatabase {
                    connection,
                    database,
                });
            }
        }
    }

    fn show_form(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.form else {
            return;
        };
        match form.show(ctx, self.keyring_available) {
            FormResult::Open => {}
            FormResult::Cancelled => self.form = None,
            FormResult::Save {
                connection,
                password,
                typed_password,
                connect,
            } => {
                self.connect_after_save = connect.then_some((connection.id, typed_password));
                self.backend.send(Command::SaveConnection {
                    connection,
                    password,
                });
            }
        }
    }

    fn show_notifications(&mut self, ui: &mut egui::Ui) {
        if self.notifications.is_empty() {
            return;
        }
        egui::Panel::bottom("notifications").show(ui, |ui| {
            let mut dismissed = None;
            for (index, error) in self.notifications.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("Dismiss").clicked() {
                        dismissed = Some(index);
                    }
                    let label = ui.colored_label(ui.visuals().error_fg_color, &error.message);
                    if let Some(detail) = &error.detail {
                        label.on_hover_text(detail);
                    }
                });
            }
            if let Some(index) = dismissed {
                self.notifications.remove(index);
            }
        });
    }

    fn show_main(&mut self, ui: &mut egui::Ui) {
        let Some(selection) = &self.selection else {
            ui.centered_and_justified(|ui| {
                ui.weak(if self.connections.is_empty() {
                    "Add a connection to get started."
                } else {
                    "Select a collection."
                });
            });
            return;
        };
        ui.heading(format!("{}.{}", selection.database, selection.collection));
        ui.horizontal(|ui| {
            let label = ui.label("Filter");
            let output = self
                .filter_editor
                .show(ui, &mut self.filter, &SpikeLanguage);
            output.response.labelled_by(label.id);
            if output.submitted {
                self.submitted_filter = Some(self.filter.clone());
            }
        });
        if let Some(filter) = &self.submitted_filter {
            ui.label(format!("Submitted: {filter}"));
        }
    }

    /// Applies the backend results that arrived since the last frame.
    pub fn process_events(&mut self) {
        let events: Vec<_> = self.backend.drain().collect();
        for event in events {
            match event.outcome {
                Outcome::Done(response) => self.handle_response(response),
                Outcome::Cancelled => self.cancelled(event.id),
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                sidebar::show(ui, &self.connections, self.selection.as_ref(), &mut actions);
            });
        self.show_notifications(ui);
        egui::CentralPanel::default().show(ui, |ui| self.show_main(ui));

        let ctx = ui.ctx().clone();
        self.show_form(&ctx);
        if let Some(dialog) = &mut self.dialog
            && !dialog.show(&ctx, &mut actions)
        {
            self.dialog = None;
        }

        for action in actions {
            self.apply(action);
        }
    }
}

impl eframe::App for NordpilApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use eframe::egui::{Key, Modifiers, Role};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use nordpil_backend::secrets::MemorySecrets;
    use nordpil_backend::store::ConnectionStore;
    use testcontainers_modules::mongo::Mongo;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::SyncRunner;

    use super::*;

    fn harness() -> Harness<'static, NordpilApp> {
        let dir = std::env::temp_dir().join(format!("nordpil-ui-{}", Uuid::new_v4()));
        let store = ConnectionStore::new(dir.join("connections.toml"));
        let backend = Backend::new(store, MemorySecrets::default());
        let app = NordpilApp::new(&egui::Context::default(), backend).unwrap();
        Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(
                |ui, app: &mut NordpilApp| {
                    app.process_events();
                    app.show(ui);
                },
                app,
            )
    }

    /// Steps the UI until `done` holds. Backend results arrive from other
    /// threads, so this polls.
    fn run_until(harness: &mut Harness<'_, NordpilApp>, done: impl Fn(&NordpilApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(harness.state()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the backend: {}",
                describe(harness.state())
            );
            std::thread::sleep(Duration::from_millis(20));
            harness.step();
        }
        harness.run();
    }

    fn describe(app: &NordpilApp) -> String {
        let states: Vec<_> = app
            .connections
            .iter()
            .map(|entry| match &entry.state {
                ConnectionState::Disconnected => "disconnected".to_owned(),
                ConnectionState::Connecting(_) => "connecting".to_owned(),
                ConnectionState::Connected(dbs) => format!("connected ({} dbs)", dbs.len()),
                ConnectionState::Failed(error) => format!("failed: {error}"),
            })
            .collect();
        let form = app
            .form
            .as_ref()
            .map(|form| (form.saving, form.error.clone()));
        let notes: Vec<_> = app.notifications.iter().map(|n| &n.message).collect();
        format!("connections {states:?}, form {form:?}, notifications {notes:?}")
    }

    fn type_into(harness: &mut Harness<'_, NordpilApp>, label: &str, text: &str) {
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .focus();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .type_text(text);
        harness.run();
    }

    fn collection_loaded(app: &NordpilApp, database: &str, collection: &str) -> bool {
        let Some(ConnectionState::Connected(databases)) =
            app.connections.first().map(|entry| &entry.state)
        else {
            return false;
        };
        databases.iter().any(|db| {
            db.info.name == database
                && matches!(&db.collections, Collections::Loaded(list)
                    if list.iter().any(|c| c.name == collection))
        })
    }

    #[test]
    #[ignore = "needs Docker"]
    fn adds_connection_connects_and_creates_database() {
        let mongo = Mongo::default().with_tag("8.0").start().unwrap();
        let port = mongo.get_host_port_ipv4(27017).unwrap();
        let mut harness = harness();
        harness.run();

        harness
            .get_by_role_and_label(Role::Button, "New connection")
            .click();
        harness.run();
        let uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true");
        type_into(&mut harness, "Connection string", &uri);
        type_into(&mut harness, "Name", "Test");
        harness
            .get_by_role_and_label(Role::Button, "Save and connect")
            .click();
        run_until(&mut harness, |app| {
            app.connections
                .first()
                .is_some_and(|entry| matches!(entry.state, ConnectionState::Connected(_)))
        });
        assert_eq!(harness.state().connections[0].saved.name, "Test");
        assert!(harness.query_all_by_label("admin").next().is_some());

        let connection = harness.state().connections[0].saved.id;
        harness.state_mut().apply(Action::CreateCollection {
            connection,
            database: None,
        });
        harness.run();
        type_into(&mut harness, "Database", "shop");
        type_into(&mut harness, "Collection", "orders");
        harness
            .get_by_role_and_label(Role::Button, "Create")
            .click();
        run_until(&mut harness, |app| {
            matches!(&app.connections[0].state, ConnectionState::Connected(dbs)
                if dbs.iter().any(|db| db.info.name == "shop"))
        });

        // What expanding the new database in the sidebar does.
        harness.state_mut().apply(Action::LoadCollections {
            connection,
            database: "shop".to_owned(),
        });
        run_until(&mut harness, |app| collection_loaded(app, "shop", "orders"));
    }
}
