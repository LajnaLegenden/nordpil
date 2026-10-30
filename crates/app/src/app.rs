use eframe::egui;
use nordpil_backend::store::SavedConnection;
use nordpil_backend::{
    Backend, BackendError, Bridge, Command, DatabaseInfo, ErrorKind, Outcome, Password, RequestId,
    Response,
};
use uuid::Uuid;

use crate::connections::{Collections, ConnectionEntry, ConnectionState, Selection};
use crate::tabs::{CollectionTab, build_query};
use crate::ui::connection_form::{ConnectionForm, FormResult};
use crate::ui::dialogs::{CreateCollectionDialog, Dialog, DropDialog, PasswordPrompt};
use crate::ui::documents::TabAction;
use crate::ui::{Action, documents, sidebar};

/// Older notifications are dropped beyond this many.
const MAX_NOTIFICATIONS: usize = 5;

pub struct NordpilApp {
    backend: Bridge<Backend>,
    connections: Vec<ConnectionEntry>,
    keyring_available: bool,
    tabs: Vec<CollectionTab>,
    active_tab: Option<usize>,
    next_tab_id: u64,
    form: Option<ConnectionForm>,
    /// Connect once the form's save succeeds.
    connect_after_save: Option<(Uuid, Option<Password>)>,
    dialog: Option<Dialog>,
    notifications: Vec<BackendError>,
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
            tabs: Vec::new(),
            active_tab: None,
            next_tab_id: 0,
            form: None,
            connect_after_save: None,
            dialog: None,
            notifications: Vec::new(),
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

    /// Closes the tabs whose collection matches, e.g. after a drop.
    fn deselect_if(&mut self, matches: impl Fn(&Selection) -> bool) {
        let active_id = self.active_tab.map(|index| self.tabs[index].id);
        for tab in self.tabs.iter().filter(|tab| matches(&tab.target)) {
            for request in [tab.pending_find, tab.pending_count].into_iter().flatten() {
                self.backend.cancel(request);
            }
        }
        self.tabs.retain(|tab| !matches(&tab.target));
        self.active_tab = active_id
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .or_else(|| self.tabs.len().checked_sub(1));
    }

    fn selection(&self) -> Option<&Selection> {
        self.active_tab.map(|index| &self.tabs[index].target)
    }

    fn open_tab(&mut self, target: Selection) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.target == target) {
            self.active_tab = Some(index);
            return;
        }
        self.next_tab_id += 1;
        let tab = CollectionTab::new(self.next_tab_id, target);
        self.backend.send(Command::LoadQueries(tab.namespace()));
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        self.active_tab = Some(index);
        self.tab_action(index, TabAction::Run);
    }

    fn close_tab(&mut self, index: usize) {
        let target = self.tabs[index].target.clone();
        self.deselect_if(|t| *t == target);
    }

    fn tab_action(&mut self, index: usize, action: TabAction) {
        let tab = &mut self.tabs[index];
        match action {
            TabAction::Run | TabAction::Reset => {
                if matches!(action, TabAction::Reset) {
                    tab.inputs = Default::default();
                }
                match build_query(&tab.inputs) {
                    Ok(applied) => {
                        if !tab.inputs.is_empty() {
                            self.backend.send(Command::RecordQuery {
                                namespace: tab.namespace(),
                                query: tab.inputs.clone(),
                            });
                        }
                        tab.input_errors.clear();
                        tab.applied = Some(applied);
                        tab.page = 0;
                        tab.count = None;
                        self.fetch_page(index, true);
                    }
                    Err(errors) => tab.input_errors = errors,
                }
            }
            TabAction::Page(page) => {
                tab.page = page;
                self.fetch_page(index, false);
            }
            TabAction::Refresh => {
                tab.count = None;
                self.fetch_page(index, true);
            }
            TabAction::Apply(inputs) => {
                tab.apply_inputs(inputs);
                self.tab_action(index, TabAction::Run);
            }
            TabAction::SetFavorite { id, favorite } => {
                self.backend.send(Command::SetQueryFavorite {
                    namespace: tab.namespace(),
                    id,
                    favorite,
                });
            }
            TabAction::DeleteQuery(id) => {
                self.backend.send(Command::DeleteQuery {
                    namespace: tab.namespace(),
                    id,
                });
            }
        }
    }

    /// Sends the find for the tab's current page, and a count if asked.
    fn fetch_page(&mut self, index: usize, count: bool) {
        let tab = &mut self.tabs[index];
        let Some(query) = tab.page_query() else {
            return;
        };
        let namespace = tab.namespace();
        for request in [tab.pending_find.take()].into_iter().flatten() {
            self.backend.cancel(request);
        }
        tab.error = None;
        tab.pending_find = Some(self.backend.send(Command::Find {
            namespace: namespace.clone(),
            query: Box::new(query.clone()),
        }));
        if count {
            if let Some(request) = tab.pending_count.take() {
                self.backend.cancel(request);
            }
            tab.pending_count = Some(self.backend.send(Command::Count {
                namespace,
                filter: query.filter,
                hint: query.hint,
            }));
        }
    }

    fn handle_response(&mut self, request: RequestId, response: Response) {
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
            Response::Documents(result) => {
                let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.pending_find == Some(request))
                else {
                    return;
                };
                tab.pending_find = None;
                match result {
                    Ok(documents) => tab.set_documents(documents),
                    Err(error) => {
                        tab.results = None;
                        tab.error = Some(error.message);
                    }
                }
            }
            Response::Queries { namespace, result } => match result {
                Ok(history) => {
                    for tab in &mut self.tabs {
                        if tab.namespace() == namespace {
                            tab.history = history.clone();
                        }
                    }
                }
                Err(error) => self.notify(error),
            },
            Response::Count(result) => {
                let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.pending_count == Some(request))
                else {
                    return;
                };
                tab.pending_count = None;
                // A failed count shouldn't hide the documents; show the total as unknown.
                tab.count = Some(result.unwrap_or(None));
            }
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
        for tab in &mut self.tabs {
            if tab.pending_find == Some(request) {
                tab.pending_find = None;
            }
            if tab.pending_count == Some(request) {
                tab.pending_count = None;
            }
        }
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
            Action::Select(selection) => self.open_tab(selection),
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
        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak(if self.connections.is_empty() {
                    "Add a connection to get started."
                } else {
                    "Select a collection."
                });
            });
            return;
        }

        let mut close = None;
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, tab) in self.tabs.iter().enumerate() {
                        let active = self.active_tab == Some(index);
                        let response = ui.selectable_label(active, tab.title());
                        if response.clicked() {
                            self.active_tab = Some(index);
                        }
                        if response.middle_clicked() {
                            close = Some(index);
                        }
                        if ui.small_button("x").on_hover_text("Close tab").clicked() {
                            close = Some(index);
                        }
                        ui.add_space(6.0);
                    }
                });
            });
        ui.separator();
        if let Some(index) = close {
            self.close_tab(index);
            return;
        }

        let Some(index) = self.active_tab else {
            return;
        };
        if let Some(action) = documents::show(ui, &mut self.tabs[index]) {
            self.tab_action(index, action);
        }
    }

    /// Applies the backend results that arrived since the last frame.
    pub fn process_events(&mut self) {
        let events: Vec<_> = self.backend.drain().collect();
        for event in events {
            match event.outcome {
                Outcome::Done(response) => self.handle_response(event.id, response),
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
                sidebar::show(ui, &self.connections, self.selection(), &mut actions);
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

        harness.state_mut().apply(Action::Select(Selection {
            connection,
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
        }));
        run_until(&mut harness, |app| {
            app.tabs
                .first()
                .is_some_and(|tab| tab.results.is_some() && tab.count.is_some())
        });
        let tab = &harness.state().tabs[0];
        assert_eq!(tab.error, None);
        assert_eq!(tab.count, Some(Some(0)));
        assert!(harness.query_by_label("No documents match.").is_some());
    }
}

#[cfg(all(test, feature = "screenshots"))]
mod screenshots {
    use eframe::egui::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use nordpil_backend::secrets::MemorySecrets;
    use nordpil_backend::store::{ConnectionColor, ConnectionStore};
    use nordpil_backend::{CollectionInfo, CollectionKind};

    use super::*;
    use crate::connections::DatabaseNode;

    fn database(name: &str, collections: &[(&str, CollectionKind)]) -> DatabaseNode {
        DatabaseNode {
            info: DatabaseInfo {
                name: name.to_owned(),
                size_on_disk: 0,
                empty: false,
            },
            collections: Collections::Loaded(
                collections
                    .iter()
                    .map(|&(name, kind)| CollectionInfo {
                        name: name.to_owned(),
                        kind,
                        capped: false,
                    })
                    .collect(),
            ),
        }
    }

    fn sample_app() -> NordpilApp {
        let dir = std::env::temp_dir().join(format!("nordpil-shot-{}", Uuid::new_v4()));
        let backend = Backend::new(
            ConnectionStore::new(dir.join("connections.toml")),
            MemorySecrets::default(),
        );
        let mut app = NordpilApp::new(&egui::Context::default(), backend).unwrap();
        // Let the initial (empty) load finish so it doesn't replace the sample data.
        while !app.keyring_available {
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.process_events();
        }
        let saved = |name: &str, color, read_only| SavedConnection {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            uri: "mongodb://localhost:27017".to_owned(),
            color,
            read_only,
        };
        let mut local =
            ConnectionEntry::new(saved("Local dev", Some(ConnectionColor::Green), false));
        local.state = ConnectionState::Connected(vec![
            database("admin", &[("system.version", CollectionKind::Collection)]),
            database(
                "shop",
                &[
                    ("customers", CollectionKind::Collection),
                    ("orders", CollectionKind::Collection),
                    ("orders_by_day", CollectionKind::View),
                    ("metrics", CollectionKind::Timeseries),
                ],
            ),
        ]);
        let production =
            ConnectionEntry::new(saved("Production", Some(ConnectionColor::Red), true));
        let target = Selection {
            connection: local.saved.id,
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
        };
        app.connections = vec![local, production];
        let mut tab = CollectionTab::new(1, target);
        tab.inputs.filter = "{ status: 'shipped', total: { $gt: 100 } }".to_owned();
        tab.applied = Some(build_query(&tab.inputs).unwrap());
        tab.count = Some(Some(1234));
        tab.set_documents(
            (0..6)
                .map(|n| {
                    bson::doc! {
                        "_id": bson::oid::ObjectId::from_bytes([n; 12]),
                        "status": "shipped",
                        "total": 100.5 + f64::from(n) * 12.25,
                        "items": [{ "sku": "A-1", "qty": 2 }, { "sku": "B-7", "qty": 1 }],
                        "customer": { "name": "Ada Lovelace", "email": "ada@example.com" },
                        "created": bson::DateTime::from_millis(1_700_000_000_000 + i64::from(n) * 86_400_000),
                    }
                })
                .collect(),
        );
        let saved = |filter: &str, sort: &str, favorite: bool| nordpil_backend::SavedQuery {
            id: Uuid::new_v4(),
            connection: Uuid::nil(),
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
            favorite,
            used_at: 0,
            query: crate::tabs::QueryInputs {
                filter: filter.to_owned(),
                sort: sort.to_owned(),
                ..Default::default()
            },
        };
        tab.history = vec![
            saved("{ status: 'pending', total: { $gt: 1000 } }", "", true),
            saved(
                "{ status: 'shipped', total: { $gt: 100 } }",
                "{ created: -1 }",
                false,
            ),
            saved("{ 'customer.email': /@example\\.com$/ }", "", false),
        ];
        app.tabs.push(tab);
        app.active_tab = Some(0);
        app
    }

    fn save(harness: &mut Harness<'_, NordpilApp>, name: &str) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let image = harness.render().unwrap();
        image.save(dir.join(format!("{name}.png"))).unwrap();
    }

    #[test]
    #[ignore = "renders with wgpu; run with `just screenshots`"]
    fn screenshot_main_window() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 650.0))
            .wgpu()
            .build_ui_state(
                |ui, app: &mut NordpilApp| {
                    app.process_events();
                    app.show(ui);
                },
                sample_app(),
            );
        harness.run();
        save(&mut harness, "main-collapsed");
        harness.get_by_label("shop").click();
        harness.run();
        save(&mut harness, "main");

        harness.state_mut().tabs[0].view = crate::tabs::View::Table;
        harness.run();
        save(&mut harness, "table");
        harness.state_mut().tabs[0].view = crate::tabs::View::Json;
        harness.state_mut().tabs[0].show_options = true;
        harness.run();
        save(&mut harness, "json-options");

        harness
            .get_by_role_and_label(Role::Button, "History")
            .click();
        harness.run();
        save(&mut harness, "history");
        harness.key_press(egui::Key::Escape);
        harness.run();

        harness.state_mut().apply(Action::NewConnection);
        harness.run();
        save(&mut harness, "connection-form");

        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        let production = harness.state().connections[1].saved.id;
        harness.state_mut().apply(Action::Drop {
            connection: production,
            database: "shop".to_owned(),
            collection: Some("orders".to_owned()),
        });
        harness.run();
        save(&mut harness, "drop-dialog");
    }
}
