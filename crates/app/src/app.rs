use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Frame, Margin, RichText, Stroke};
use nordpil_backend::navigation::{KnownDatabase, Recent};
use nordpil_backend::references::{DatabaseMappings, Mapping};
use nordpil_backend::settings::{GotoScope, Settings};
use nordpil_backend::store::SavedConnection;
use nordpil_backend::{
    Backend, BackendError, Bridge, Command, DatabaseInfo, ErrorKind, Outcome, Password, RequestId,
    Response,
};
use uuid::Uuid;

use crate::connections::{Collections, ConnectionEntry, ConnectionState, Selection};
use crate::lookup::References;
use crate::shell::{Body, Ending, FileJob, PageRequest, Running};
use crate::shortcuts::{self, AppCommand};
use crate::tabs::{CollectionTab, Panel, QueryInputs, Results, View, build_query, history_entry};
use crate::ui::connection_form::{ConnectionForm, FormResult};
use crate::ui::dialogs::{CreateCollectionDialog, Dialog, DropDialog, PasswordPrompt};
use crate::ui::document_dialogs::{self, DocumentDialog};
use crate::ui::documents::TabAction;
use crate::ui::goto::{self, GotoPalette};
use crate::ui::index_dialog::{self, IndexDialog};
use crate::ui::indexes::{self, IndexAction};
use crate::ui::mappings_dialog::{self, MappingsDialog, Source};
use crate::ui::settings_dialog::{self, SettingsDialog};
use crate::ui::shell::ShellAction;
use crate::ui::shortcuts_dialog;
use crate::ui::transfer_dialog::{self, TransferDialog};
use crate::ui::{Action, chrome, documents, sidebar};
use crate::update::{self, Updater};
use crate::widgets::kit::{self, Tone};
use crate::{icons, theme};

/// What a script key does on `panel`: nothing off the Shell panel.
fn script_action(panel: Panel, command: AppCommand) -> Option<ShellAction> {
    if panel != Panel::Shell {
        return None;
    }
    match command {
        AppCommand::SaveScript => Some(ShellAction::File(FileJob::Save)),
        AppCommand::OpenScript => Some(ShellAction::File(FileJob::Open)),
        AppCommand::StopScript => Some(ShellAction::Stop),
        _ => None,
    }
}

/// The edit dialog shows a document's whole text, which past the preview's
/// size gets too slow to edit.
fn too_large_to_edit(document: &bson::Document) -> bool {
    nordpil_query::format_document(document, nordpil_query::Style::Pretty(2)).len()
        > nordpil_backend::PREVIEW_BYTES
}

/// A modal that holds back app-wide keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Settings,
    GoTo,
    Shortcuts,
    Other,
}

enum UpdateClick {
    Install,
    Restart,
    Dismiss,
}

/// Older notifications are dropped beyond this many.
const MAX_NOTIFICATIONS: usize = 5;
/// Recently opened collections kept for Ctrl+K.
const MAX_RECENTS: usize = 50;
/// Points per mouse wheel notch: egui's 40 felt slow.
const LINE_SCROLL_SPEED: f32 = 60.0;
/// Closed tabs Ctrl+Shift+T can bring back.
const MAX_CLOSED: usize = 10;

/// What Ctrl+Shift+T brings back of a closed tab. The shell script comes
/// back by itself, from the draft saved on close.
struct ClosedTab {
    target: Selection,
    panel: Panel,
    inputs: QueryInputs,
    show_options: bool,
    view: View,
    page_size: u64,
}

impl ClosedTab {
    fn of(tab: &CollectionTab) -> Self {
        Self {
            target: tab.target.clone(),
            panel: tab.panel,
            inputs: tab.inputs.clone(),
            show_options: tab.show_options,
            view: tab.view,
            page_size: tab.page_size,
        }
    }
}

/// A tab to open once its connection, being connected, is open.
enum PendingOpen {
    /// From Ctrl+K: the collection's tab, opened if it has none.
    Collection(Selection),
    /// From Ctrl+Shift+T.
    Reopen(Box<ClosedTab>),
}

impl PendingOpen {
    fn connection(&self) -> Uuid {
        match self {
            Self::Collection(target) => target.connection,
            Self::Reopen(closed) => closed.target.connection,
        }
    }
}

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
    document_dialog: Option<DocumentDialog>,
    index_dialog: Option<IndexDialog>,
    transfer_dialog: Option<TransferDialog>,
    notifications: Vec<BackendError>,
    settings: Settings,
    settings_dialog: Option<SettingsDialog>,
    /// The F1 sheet of keyboard shortcuts.
    shortcuts_sheet: bool,
    /// Fields the user linked to collections, per database.
    mappings: Vec<DatabaseMappings>,
    mappings_dialog: Option<MappingsDialog>,
    goto: Option<GotoPalette>,
    /// Newest first.
    recents: Vec<Recent>,
    /// Collection names remembered for saved connections, for Ctrl+K.
    known: Vec<KnownDatabase>,
    /// Opened once its connection, being connected, is open.
    pending_open: Option<PendingOpen>,
    /// Tabs the user closed, newest last, for Ctrl+Shift+T.
    closed: Vec<ClosedTab>,
    /// Off in development builds and tests.
    updater: Option<Updater>,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::MacMenu>,
}

impl NordpilApp {
    /// `ctx` is repainted whenever a backend result arrives, and gets the
    /// theme and the saved settings.
    pub fn new(ctx: &egui::Context, backend: Backend) -> std::io::Result<Self> {
        let (settings, settings_error) = match backend.load_settings() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        theme::install(ctx);
        theme::apply_settings(ctx, &settings);
        ctx.options_mut(|options| options.input_options.line_scroll_speed = LINE_SCROLL_SPEED);
        let ctx = ctx.clone();
        let mut backend = Bridge::new(backend, move || ctx.request_repaint())?;
        backend.send(Command::LoadConnections);
        backend.send(Command::LoadNavigation);
        backend.send(Command::LoadMappings);

        let mut app = Self {
            backend,
            connections: Vec::new(),
            keyring_available: false,
            tabs: Vec::new(),
            active_tab: None,
            next_tab_id: 0,
            form: None,
            connect_after_save: None,
            dialog: None,
            document_dialog: None,
            index_dialog: None,
            transfer_dialog: None,
            notifications: Vec::new(),
            settings,
            settings_dialog: None,
            shortcuts_sheet: false,
            mappings: Vec::new(),
            mappings_dialog: None,
            goto: None,
            recents: Vec::new(),
            known: Vec::new(),
            pending_open: None,
            closed: Vec::new(),
            updater: None,
            #[cfg(target_os = "macos")]
            menu: None,
        };
        if let Some(error) = settings_error {
            app.notify(error);
        }
        Ok(app)
    }

    /// Puts the app's menus in the macOS menu bar.
    #[cfg(target_os = "macos")]
    pub fn install_menu(&mut self, ctx: &egui::Context) {
        self.menu = Some(crate::menu::MacMenu::install(ctx));
    }

    /// Checks GitHub for newer releases, as the `updates` setting says.
    pub fn enable_updates(&mut self, ctx: &egui::Context) {
        self.updater = Some(Updater::new(ctx.clone()));
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
        self.close_tabs(|tab| matches(&tab.target));
    }

    fn close_tabs(&mut self, matches: impl Fn(&CollectionTab) -> bool) {
        let active_id = self.active_tab.map(|index| self.tabs[index].id);
        for tab in self.tabs.iter_mut().filter(|tab| matches(tab)) {
            let running = tab.shell.running.as_ref().map(|running| running.request);
            for request in [tab.pending_find, tab.pending_count, running]
                .into_iter()
                .flatten()
            {
                self.backend.cancel(request);
            }
            if let Some(text) = tab.shell.draft_to_save(true) {
                self.backend.send(Command::SaveDraft {
                    namespace: tab.namespace(),
                    text,
                });
            }
        }
        self.tabs.retain(|tab| !matches(tab));
        self.active_tab = active_id
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .or_else(|| self.tabs.len().checked_sub(1));
    }

    fn selection(&self) -> Option<&Selection> {
        self.active_tab.map(|index| &self.tabs[index].target)
    }

    /// Shows the collection's tab, opening one if there is none.
    fn open_tab(&mut self, target: Selection) {
        self.record_recent(&target);
        if let Some(index) = self.tabs.iter().position(|tab| tab.target == target) {
            self.active_tab = Some(index);
            return;
        }
        self.push_tab(target, String::new());
    }

    /// Opens another tab even when the collection has one, and runs `filter`.
    fn push_tab(&mut self, target: Selection, filter: String) {
        self.add_tab(target, |tab| tab.inputs.filter = filter);
    }

    /// Opens another tab, lets `setup` change it, then runs its query.
    fn add_tab(&mut self, target: Selection, setup: impl FnOnce(&mut CollectionTab)) {
        self.load_references(&target);
        self.next_tab_id += 1;
        let mut tab = CollectionTab::new(self.next_tab_id, target);
        tab.panel = self.settings.last_panel;
        setup(&mut tab);
        self.backend.send(Command::LoadQueries(tab.namespace()));
        self.backend.send(Command::LoadScripts(tab.namespace()));
        self.backend.send(Command::SampleFields(tab.namespace()));
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        self.active_tab = Some(index);
        self.tab_action(index, TabAction::Run);
    }

    /// With lookups on, lists the collections of the tab's database if the
    /// sidebar hasn't yet, so id fields can be matched to them.
    fn load_references(&mut self, target: &Selection) {
        if !self.settings.look_up_references {
            return;
        }
        let not_loaded = self
            .entry(target.connection)
            .and_then(|entry| entry.database(&target.database))
            .is_some_and(|node| matches!(node.collections, Collections::NotLoaded));
        if not_loaded {
            self.apply(Action::LoadCollections {
                connection: target.connection,
                database: target.database.clone(),
            });
        }
    }

    /// Where id fields in the tab can be looked up; `None` when lookups
    /// are off.
    fn references(&self, index: usize) -> Option<References> {
        let target = &self.tabs[index].target;
        self.references_in(target.connection, &target.database)
    }

    /// Where id fields of documents in `database` can be looked up.
    fn references_in(&self, connection: Uuid, database: &str) -> Option<References> {
        if !self.settings.look_up_references {
            return None;
        }
        Some(References {
            collections: self.collection_names(connection, database),
            mappings: self.database_mappings(connection, database).to_vec(),
        })
    }

    /// Empty until the database's collections are loaded.
    fn collection_names(&self, connection: Uuid, database: &str) -> Vec<String> {
        match self
            .entry(connection)
            .and_then(|entry| entry.database(database))
            .map(|node| &node.collections)
        {
            Some(Collections::Loaded(collections)) => collections
                .iter()
                .map(|collection| collection.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn database_mappings(&self, connection: Uuid, database: &str) -> &[Mapping] {
        self.mappings
            .iter()
            .find(|mappings| mappings.connection == connection && mappings.database == database)
            .map_or(&[], |mappings| &mappings.mappings)
    }

    /// E.g. `Local dev / shop`; `None` for a deleted connection.
    fn location(&self, connection: Uuid, database: &str) -> Option<String> {
        self.entry(connection)
            .map(|entry| format!("{} / {database}", entry.saved.name))
    }

    /// Replaces a database's linked fields and saves them all.
    fn set_mappings(&mut self, connection: Uuid, database: String, mappings: Vec<Mapping>) {
        self.mappings
            .retain(|other| !(other.connection == connection && other.database == database));
        if !mappings.is_empty() {
            self.mappings.push(DatabaseMappings {
                connection,
                database,
                mappings,
            });
        }
        self.backend
            .send(Command::SaveMappings(self.mappings.clone()));
    }

    fn show_mappings_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &self.mappings_dialog else {
            return;
        };
        let collections = self.collection_names(dialog.connection, &dialog.database);
        let Some(dialog) = &mut self.mappings_dialog else {
            return;
        };
        match dialog.show(ctx, &collections) {
            mappings_dialog::Outcome::Open => {}
            mappings_dialog::Outcome::Cancel => self.mappings_dialog = None,
            mappings_dialog::Outcome::Save(mappings) => {
                let (connection, database) = (dialog.connection, dialog.database.clone());
                self.mappings_dialog = None;
                self.set_mappings(connection, database, mappings);
            }
        }
    }

    /// Moves the collection to the top of the recents and saves them.
    fn record_recent(&mut self, target: &Selection) {
        let opened_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        self.recents.retain(|recent| {
            !(recent.connection == target.connection
                && recent.database == target.database
                && recent.collection == target.collection)
        });
        self.recents.insert(
            0,
            Recent {
                connection: target.connection,
                database: target.database.clone(),
                collection: target.collection.clone(),
                opened_at,
            },
        );
        self.recents.truncate(MAX_RECENTS);
        self.backend
            .send(Command::SaveRecents(self.recents.clone()));
    }

    /// Opens a collection from Ctrl+K, connecting first if needed.
    fn go_to(&mut self, target: Selection) {
        match self.entry(target.connection).map(|entry| &entry.state) {
            Some(ConnectionState::Connected(_)) => self.open_tab(target),
            Some(ConnectionState::Connecting(_)) => {
                self.pending_open = Some(PendingOpen::Collection(target));
            }
            Some(_) => {
                let connection = target.connection;
                self.pending_open = Some(PendingOpen::Collection(target));
                self.connect(connection, None, false);
            }
            None => {}
        }
    }

    /// What Ctrl+K can open: the collections of open connections, and with
    /// [`GotoScope::AllSaved`], the remembered and recent ones of the rest.
    fn goto_candidates(&self) -> Vec<goto::Candidate> {
        let all_saved = self.settings.goto_scope == GotoScope::AllSaved;
        let mut candidates = Vec::new();
        for entry in &self.connections {
            let id = entry.saved.id;
            let color = entry
                .saved
                .color
                .map_or(theme::of(&egui::Visuals::dark()).subtle, sidebar::color32);
            let mut push = |database: &str, collection: &str, kind, connected| {
                let target = Selection {
                    connection: id,
                    database: database.to_owned(),
                    collection: collection.to_owned(),
                };
                if candidates
                    .iter()
                    .any(|c: &goto::Candidate| c.target == target)
                {
                    return;
                }
                candidates.push(goto::Candidate {
                    open: self.tabs.iter().any(|tab| tab.target == target),
                    target,
                    connection_name: entry.saved.name.clone(),
                    color,
                    kind,
                    connected,
                });
            };
            match &entry.state {
                ConnectionState::Connected(databases) => {
                    for database in databases {
                        if let Collections::Loaded(collections) = &database.collections {
                            for collection in collections {
                                push(&database.info.name, &collection.name, collection.kind, true);
                            }
                        }
                    }
                }
                _ if all_saved => {
                    for known in self.known.iter().filter(|known| known.connection == id) {
                        for collection in &known.collections {
                            push(&known.database, &collection.name, collection.kind, false);
                        }
                    }
                }
                _ => continue,
            }
            let connected = matches!(entry.state, ConnectionState::Connected(_));
            for recent in self.recents.iter().filter(|recent| recent.connection == id) {
                push(
                    &recent.database,
                    &recent.collection,
                    nordpil_backend::CollectionKind::Collection,
                    connected,
                );
            }
        }
        candidates
    }

    fn set_settings(&mut self, ctx: &egui::Context, settings: Settings) {
        let scope_changed = settings.goto_scope != self.settings.goto_scope;
        let lookups_turned_on = settings.look_up_references && !self.settings.look_up_references;
        self.settings = settings;
        theme::apply_settings(ctx, &settings);
        self.backend.send(Command::SaveSettings(settings));
        if lookups_turned_on {
            let targets: Vec<Selection> = self.tabs.iter().map(|tab| tab.target.clone()).collect();
            for target in targets {
                self.load_references(&target);
            }
        }
        if !scope_changed {
            return;
        }
        match settings.goto_scope {
            GotoScope::OpenOnly => {
                self.known.clear();
                self.backend.send(Command::ForgetCollections(None));
            }
            GotoScope::AllSaved => {
                let connected: Vec<Uuid> = self
                    .connections
                    .iter()
                    .filter(|entry| matches!(entry.state, ConnectionState::Connected(_)))
                    .map(|entry| entry.saved.id)
                    .collect();
                for connection in connected {
                    self.backend.send(Command::IndexCollections {
                        connection,
                        remember: true,
                    });
                }
            }
        }
    }

    /// Closes a tab the user asked to close, keeping it for Ctrl+Shift+T.
    fn close_tab(&mut self, index: usize) {
        self.closed.push(ClosedTab::of(&self.tabs[index]));
        if self.closed.len() > MAX_CLOSED {
            self.closed.remove(0);
        }
        let id = self.tabs[index].id;
        self.close_tabs(|tab| tab.id == id);
    }

    /// Ctrl+R: Documents runs its query again, staying on the page unless
    /// the inputs changed since; Indexes reloads. The shell never reruns.
    fn refresh(&mut self, index: usize) {
        let tab = &self.tabs[index];
        match tab.panel {
            Panel::Documents => {
                let unchanged = build_query(&tab.inputs)
                    .is_ok_and(|applied| tab.applied.as_ref() == Some(&applied));
                let action = if unchanged {
                    TabAction::Refresh
                } else {
                    TabAction::Run
                };
                self.tab_action(index, action);
            }
            Panel::Indexes => self.index_action(index, IndexAction::Refresh),
            Panel::Shell => {}
        }
    }

    /// Brings back the last closed tab whose connection still exists,
    /// connecting first if needed.
    fn reopen_tab(&mut self) {
        while let Some(closed) = self.closed.pop() {
            let connection = closed.target.connection;
            match self.entry(connection).map(|entry| &entry.state) {
                None => continue,
                Some(ConnectionState::Connected(_)) => self.restore_tab(closed),
                Some(ConnectionState::Connecting(_)) => {
                    self.pending_open = Some(PendingOpen::Reopen(Box::new(closed)));
                }
                Some(_) => {
                    self.pending_open = Some(PendingOpen::Reopen(Box::new(closed)));
                    self.connect(connection, None, false);
                }
            }
            return;
        }
    }

    fn restore_tab(&mut self, closed: ClosedTab) {
        self.record_recent(&closed.target);
        let target = closed.target.clone();
        self.add_tab(target, |tab| {
            tab.panel = closed.panel;
            tab.inputs = closed.inputs;
            tab.show_options = closed.show_options;
            tab.view = closed.view;
            tab.page_size = closed.page_size;
        });
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
                        let entry = history_entry(&tab.inputs);
                        if !entry.is_empty() {
                            self.backend.send(Command::RecordQuery {
                                namespace: tab.namespace(),
                                query: entry,
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
            TabAction::Stop => {
                // Dropping the request closes its connection, and the server
                // then ends the operation.
                for request in [tab.pending_find.take(), tab.pending_count.take()]
                    .into_iter()
                    .flatten()
                {
                    self.backend.cancel(request);
                }
                tab.results = None;
                tab.stopped = true;
            }
            TabAction::Refresh => {
                self.backend.send(Command::SampleFields(tab.namespace()));
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
            TabAction::Export => {
                let query = tab
                    .applied
                    .as_ref()
                    .map(|applied| nordpil_backend::FindQuery {
                        skip: applied.skip,
                        limit: i64::try_from(applied.limit).unwrap_or(0),
                        max_time: None,
                        ..applied.query.clone()
                    });
                let fields: Vec<String> = tab.fields.iter().map(|f| f.path.clone()).collect();
                self.transfer_dialog =
                    Some(TransferDialog::export(tab.namespace(), query, &fields));
            }
            TabAction::Import => {
                self.transfer_dialog = Some(TransferDialog::import(tab.namespace()));
            }
            TabAction::Insert => {
                self.document_dialog = Some(DocumentDialog::new(
                    tab.namespace(),
                    document_dialogs::Kind::Insert,
                ));
            }
            TabAction::Edit(document) | TabAction::Clone(document)
                if tab
                    .results
                    .as_ref()
                    .and_then(|results| results.documents.get(document))
                    .is_some_and(too_large_to_edit) =>
            {
                self.notify(BackendError::new(
                    "This document is too large to edit here. Use a bulk update with a filter on its _id.",
                ));
            }
            TabAction::Edit(document)
            | TabAction::Clone(document)
            | TabAction::Delete(document) => {
                let Some(document_value) = tab
                    .results
                    .as_ref()
                    .and_then(|results| results.documents.get(document))
                else {
                    return;
                };
                let namespace = tab.namespace();
                self.document_dialog = Some(match action {
                    TabAction::Clone(_) => DocumentDialog::clone_of(namespace, document_value),
                    TabAction::Delete(_) => {
                        let Some(id) = document_value.get("_id").cloned() else {
                            self.notify(BackendError::new(
                                "Documents without an _id can't be deleted here.",
                            ));
                            return;
                        };
                        DocumentDialog::new(namespace, document_dialogs::Kind::Delete { id })
                    }
                    _ => DocumentDialog::new(
                        namespace,
                        document_dialogs::Kind::Edit {
                            original: Box::new(document_value.clone()),
                        },
                    ),
                });
            }
            // `documents::show` puts the field into the filter itself.
            TabAction::AddToFilter { .. } => {}
            TabAction::LinkField(key) => {
                let Selection {
                    connection,
                    database,
                    ..
                } = tab.target.clone();
                let location = self.location(connection, &database).unwrap_or_default();
                let mappings = self.database_mappings(connection, &database).to_vec();
                self.mappings_dialog = Some(MappingsDialog::link(
                    connection, database, location, key, mappings,
                ));
            }
            TabAction::Lookup { collection, filter } => {
                let target = Selection {
                    collection,
                    ..tab.target.clone()
                };
                let filter = nordpil_query::format_document(&filter, nordpil_query::Style::Compact);
                self.record_recent(&target);
                self.push_tab(target, filter);
            }
            TabAction::UpdateMany | TabAction::DeleteMany => {
                let Some(applied) = &tab.applied else {
                    return;
                };
                let filter = applied.query.filter.clone();
                let namespace = tab.namespace();
                let kind = if matches!(action, TabAction::UpdateMany) {
                    document_dialogs::Kind::UpdateMany {
                        filter: filter.clone(),
                    }
                } else {
                    document_dialogs::Kind::DeleteMany {
                        filter: filter.clone(),
                    }
                };
                let mut dialog = DocumentDialog::new(namespace.clone(), kind);
                dialog.count_request = Some(self.backend.send(Command::Count {
                    namespace,
                    filter,
                    hint: None,
                }));
                self.document_dialog = Some(dialog);
            }
        }
    }

    /// Runs `script` in the tab's shell, unless a run is going.
    fn run_script(&mut self, index: usize, script: String) {
        let namespace = self.tabs[index].namespace();
        let shell = &mut self.tabs[index].shell;
        if script.trim().is_empty() || shell.running.is_some() {
            return;
        }
        let run = nordpil_backend::ScriptRun::new();
        let request = self.backend.send(Command::RunScript {
            namespace: namespace.clone(),
            script: script.clone(),
            run: Arc::clone(&run),
        });
        shell.start(script.clone(), Running { request, run });
        self.backend
            .send(Command::RecordScript { namespace, script });
    }

    fn shell_action(&mut self, ctx: &egui::Context, index: usize, action: ShellAction) {
        let namespace = self.tabs[index].namespace();
        let page_size = self.tabs[index].page_size;
        let shell = &mut self.tabs[index].shell;
        match action {
            ShellAction::Run(script) => self.run_script(index, script),
            ShellAction::SaveLayout => {
                self.backend.send(Command::SaveSettings(self.settings));
            }
            ShellAction::Lookup {
                database,
                collection,
                filter,
            } => {
                let target = Selection {
                    connection: namespace.connection,
                    database,
                    collection,
                };
                let statement = crate::shell::find_statement(&target.collection, &filter);
                let filter = nordpil_query::format_document(&filter, nordpil_query::Style::Compact);
                self.record_recent(&target);
                self.push_tab(target, filter);
                if let Some(tab) = self.tabs.last_mut()
                    && tab.panel == Panel::Shell
                {
                    tab.shell.queued = Some(statement);
                }
            }
            ShellAction::Stop => {
                if let Some(running) = &shell.running {
                    self.backend.cancel(running.request);
                }
            }
            ShellAction::Page { result, page } => {
                let Some(entry) = shell.results.get_mut(result) else {
                    return;
                };
                match &mut entry.body {
                    Body::Query(query) => query.page = page,
                    Body::Documents {
                        documents,
                        page: current_page,
                        current,
                    } => {
                        *current_page = page;
                        *current = crate::shell::page_of(documents, page, page_size);
                        return;
                    }
                    Body::Value { .. } => return,
                }
                self.fetch_shell_page(index, result);
            }
            ShellAction::SetFavorite { id, favorite } => {
                self.backend.send(Command::SetScriptFavorite {
                    namespace,
                    id,
                    favorite,
                });
            }
            ShellAction::Delete(id) => {
                self.backend.send(Command::DeleteScript { namespace, id });
            }
            ShellAction::Sample(collection) => {
                if shell.sampling.insert(collection.clone()) {
                    self.backend
                        .send(Command::SampleFields(nordpil_backend::Namespace {
                            collection,
                            ..namespace
                        }));
                }
            }
            ShellAction::File(job) => {
                if shell.picker.is_none() {
                    let receiver = crate::shell::pick_file(ctx, job, shell.text.clone());
                    shell.picker = Some((job, receiver));
                }
            }
        }
    }

    /// Fetches the current page of a shell result that is a query.
    fn fetch_shell_page(&mut self, index: usize, result: usize) {
        let tab = &mut self.tabs[index];
        let connection = tab.target.connection;
        let page_size = tab.page_size;
        let Some(Body::Query(page)) = tab.shell.results.get_mut(result).map(|r| &mut r.body) else {
            return;
        };
        if let Some(request) = page.pending.take() {
            self.backend.cancel(request);
        }
        page.error = None;
        let namespace = nordpil_backend::Namespace {
            connection,
            database: page.query.namespace.database.clone(),
            collection: page.query.namespace.collection.clone(),
        };
        let command = match page.request(page_size) {
            PageRequest::Find(query) => Command::Find { namespace, query },
            PageRequest::Aggregate(query) => Command::Aggregate { namespace, query },
        };
        page.pending = Some(self.backend.send(command));
    }

    fn shell_page_arrived(
        &mut self,
        request: RequestId,
        result: Result<Vec<bson::Document>, BackendError>,
    ) {
        for tab in &mut self.tabs {
            if let Some(page) = tab.shell.result_for_request(request) {
                page.pending = None;
                match result {
                    Ok(documents) => page.results = Some(Results::new(documents)),
                    Err(error) => {
                        page.results = None;
                        page.error = Some(error.message);
                    }
                }
                return;
            }
        }
    }

    fn running_tab(&self, request: RequestId) -> Option<usize> {
        self.tabs.iter().position(|tab| {
            tab.shell
                .running
                .as_ref()
                .is_some_and(|running| running.request == request)
        })
    }

    fn script_finished(
        &mut self,
        request: RequestId,
        result: Result<nordpil_shell::Outcome, BackendError>,
    ) {
        let Some(index) = self.running_tab(request) else {
            return;
        };
        let ending = match result {
            Ok(nordpil_shell::Outcome::Finished) => None,
            Ok(nordpil_shell::Outcome::Failed(error)) => Some(Ending::Failed(error)),
            Ok(nordpil_shell::Outcome::Cancelled) => Some(Ending::Stopped),
            Ok(nordpil_shell::Outcome::Declined) => Some(Ending::Declined),
            Err(error) => Some(Ending::Failed(nordpil_shell::ScriptError {
                message: error.message,
                statement: None,
                line: None,
                column: None,
            })),
        };
        self.finish_script(index, ending);
    }

    /// Takes the run's last results, then marks it done.
    fn finish_script(&mut self, index: usize, ending: Option<Ending>) {
        let page_size = self.tabs[index].page_size;
        for result in self.tabs[index].shell.take_events(page_size) {
            self.fetch_shell_page(index, result);
        }
        self.tabs[index].shell.finish(ending);
    }

    /// Each frame: results of running scripts, drafts to save and file
    /// dialogs that closed.
    fn poll_shells(&mut self, ctx: &egui::Context) {
        for index in 0..self.tabs.len() {
            let page_size = self.tabs[index].page_size;
            let shell = &mut self.tabs[index].shell;
            if shell.running.is_some() {
                // Results come from another thread without waking the UI.
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            for result in shell.take_events(page_size) {
                self.fetch_shell_page(index, result);
            }
            let shell = &mut self.tabs[index].shell;
            if shell.edited_at.is_some() {
                ctx.request_repaint_after(crate::shell::DRAFT_DELAY);
            }
            if let Some(text) = shell.draft_to_save(false) {
                let namespace = self.tabs[index].namespace();
                self.backend.send(Command::SaveDraft { namespace, text });
            }
            let shell = &mut self.tabs[index].shell;
            let Some((job, receiver)) = &shell.picker else {
                continue;
            };
            let Ok(result) = receiver.try_recv() else {
                continue;
            };
            let job = *job;
            shell.picker = None;
            match result {
                Some(Ok((_, text))) if job == FileJob::Open => {
                    shell.text = text;
                    shell.edited();
                }
                Some(Err(message)) => self.notify(BackendError::new(message)),
                _ => {}
            }
        }
    }

    /// Asks before a script's first write, for whichever tab's run waits.
    fn show_write_confirmation(&mut self, ctx: &egui::Context) {
        let Some((tab, run, summary)) = self.tabs.iter().find_map(|tab| {
            let running = tab.shell.running.as_ref()?;
            let summary = running.run.pending_write()?;
            Some((tab, Arc::clone(&running.run), summary))
        }) else {
            return;
        };
        let namespace = format!(
            "{}.{}",
            summary.namespace.database, summary.namespace.collection
        );
        let count = documents::plural(
            usize::try_from(summary.documents).unwrap_or(usize::MAX),
            "document",
        );
        let what = match summary.operation.as_str() {
            "insertOne" | "insertMany" => {
                format!("{} adds {count} to {namespace}.", summary.operation)
            }
            "bulkWrite" => format!(
                "bulkWrite runs {} on {namespace}.",
                documents::plural(
                    usize::try_from(summary.documents).unwrap_or(usize::MAX),
                    "operation"
                )
            ),
            operation => format!("{operation} on {namespace} matches {count} now."),
        };
        let subtitle = format!("Shell in {}", tab.target.collection);
        let mut answer = None;
        let modal = kit::modal(ctx, egui::Id::unique("shell_write")).show(ctx, |ui| {
            kit::dialog_width(ui, 440.0);
            kit::dialog_title(ui, "Allow writes in this run?", Some(&subtitle));
            ui.label(what);
            ui.add_space(4.0);
            ui.label(
                RichText::new("Later writes in this run won't ask again.")
                    .color(theme::palette(ui).muted),
            );
            kit::dialog_footer(ui, |ui| {
                if kit::button(
                    ui,
                    kit::Tone::Danger,
                    None,
                    "Allow writes",
                    kit::DIALOG_HEIGHT,
                )
                .clicked()
                {
                    answer = Some(true);
                }
                if kit::button(
                    ui,
                    kit::Tone::Secondary,
                    None,
                    "Stop run",
                    kit::DIALOG_HEIGHT,
                )
                .clicked()
                {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() {
            answer = answer.or(Some(false));
        }
        if let Some(allow) = answer {
            run.answer(allow);
        }
    }

    /// Lists the database's collections if the sidebar hasn't yet.
    fn ensure_collections(&mut self, target: &Selection) {
        let not_loaded = self
            .entry(target.connection)
            .and_then(|entry| entry.database(&target.database))
            .is_some_and(|node| matches!(node.collections, Collections::NotLoaded));
        if not_loaded {
            self.apply(Action::LoadCollections {
                connection: target.connection,
                database: target.database.clone(),
            });
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
        tab.stopped = false;
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
                    self.recents.retain(|recent| recent.connection != id);
                    // The backend dropped them from its file too.
                    self.mappings.retain(|database| database.connection != id);
                    self.known.retain(|known| known.connection != id);
                    self.backend
                        .send(Command::SaveRecents(self.recents.clone()));
                    self.backend.send(Command::ForgetCollections(Some(id)));
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
                    self.shell_page_arrived(request, result);
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
            Response::Fields { namespace, result } => match result {
                Ok(fields) => {
                    let hints: Vec<_> = fields
                        .into_iter()
                        .map(|field| nordpil_query::FieldHint {
                            path: field.path,
                            detail: field.types.join(", "),
                        })
                        .collect();
                    for tab in &mut self.tabs {
                        if tab.namespace() == namespace {
                            tab.fields = hints.clone();
                        }
                        let same_database = tab.target.connection == namespace.connection
                            && tab.target.database == namespace.database;
                        if same_database && tab.shell.sampling.contains(&namespace.collection) {
                            tab.shell
                                .fields
                                .insert(namespace.collection.clone(), hints.clone());
                        }
                    }
                }
                // Autocomplete then just lacks field names; not worth an error.
                Err(error) => tracing::warn!(%error, "could not sample fields"),
            },
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
            Response::Count(result)
                if self
                    .document_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.count_request == Some(request)) =>
            {
                if let Some(dialog) = &mut self.document_dialog {
                    dialog.count_request = None;
                    dialog.count = Some(result.unwrap_or(None));
                }
            }
            Response::Indexes { namespace, result } => {
                for tab in &mut self.tabs {
                    if tab.namespace() == namespace {
                        tab.indexes_request = None;
                        tab.indexes = Some(result.clone().map_err(|error| error.message));
                    }
                }
            }
            Response::IndexChanged { namespace, result } => {
                let ours = self
                    .index_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.pending == Some(request));
                match result {
                    Ok(()) => {
                        if ours {
                            self.index_dialog = None;
                        }
                        self.load_indexes(&namespace);
                    }
                    Err(error) => match &mut self.index_dialog {
                        Some(dialog) if ours => {
                            dialog.pending = None;
                            dialog.error = Some(error.message);
                        }
                        _ => self.notify(error),
                    },
                }
            }
            Response::Exported(result) => {
                if let Some(dialog) = &mut self.transfer_dialog {
                    let path = dialog.path().to_owned();
                    dialog.finished(result.map(|n| format!("Exported {n} documents to {path}.")));
                }
            }
            Response::Imported { namespace, result } => {
                let imported = result.is_ok();
                if let Some(dialog) = &mut self.transfer_dialog {
                    dialog
                        .finished(result.map(|summary| transfer_dialog::import_message(&summary)));
                }
                if imported {
                    let indexes: Vec<_> = (0..self.tabs.len())
                        .filter(|&index| self.tabs[index].namespace() == namespace)
                        .collect();
                    for index in indexes {
                        self.tab_action(index, TabAction::Refresh);
                    }
                }
            }
            Response::Written { namespace, result } => {
                let ours = self
                    .document_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.pending == Some(request));
                match result {
                    Ok(_) => {
                        if ours {
                            self.document_dialog = None;
                        }
                        let indexes: Vec<_> = (0..self.tabs.len())
                            .filter(|&index| self.tabs[index].namespace() == namespace)
                            .collect();
                        for index in indexes {
                            self.tab_action(index, TabAction::Refresh);
                        }
                    }
                    Err(error) => match &mut self.document_dialog {
                        Some(dialog) if ours => {
                            dialog.pending = None;
                            dialog.error = Some(error.message);
                        }
                        _ => self.notify(error),
                    },
                }
            }
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
            Response::Navigation(result) => match result {
                Ok(navigation) => {
                    self.recents = navigation.recents;
                    if self.settings.goto_scope == GotoScope::AllSaved {
                        self.known = navigation.databases;
                    }
                }
                Err(error) => self.notify(error),
            },
            Response::Mappings(result) => match result {
                Ok(mappings) => self.mappings = mappings,
                Err(error) => self.notify(error),
            },
            Response::CollectionIndex { connection, result } => match result {
                Ok(databases) => self.collections_indexed(connection, databases),
                // Ctrl+K then just lacks these collections; not worth an error.
                Err(error) => tracing::warn!(%error, "could not list all collections"),
            },
            Response::Stored(result) => {
                if let Err(error) = result {
                    self.notify(error);
                }
            }
            Response::ScriptFinished(result) => self.script_finished(request, result),
            Response::Scripts { namespace, result } => match result {
                Ok(scripts) => {
                    let mut queued = Vec::new();
                    for (index, tab) in self.tabs.iter_mut().enumerate() {
                        if tab.namespace() == namespace {
                            tab.shell.history = scripts.history.clone();
                            tab.shell
                                .load_draft(scripts.draft.clone(), &tab.target.collection);
                            if let Some(statement) = tab.shell.append_queued() {
                                queued.push((index, statement));
                            }
                        }
                    }
                    for (index, statement) in queued {
                        self.run_script(index, statement);
                    }
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
        let remember = self.settings.goto_scope == GotoScope::AllSaved;
        let Some(entry) = self.entry_mut(id) else {
            return;
        };
        match result {
            Ok(databases) => {
                entry.set_databases(databases);
                self.backend.send(Command::IndexCollections {
                    connection: id,
                    remember,
                });
                match self
                    .pending_open
                    .take_if(|pending| pending.connection() == id)
                {
                    Some(PendingOpen::Collection(target)) => self.open_tab(target),
                    Some(PendingOpen::Reopen(closed)) => self.restore_tab(*closed),
                    None => {}
                }
            }
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
                if self
                    .pending_open
                    .as_ref()
                    .is_some_and(|pending| pending.connection() == id)
                {
                    self.pending_open = None;
                }
                self.notify(error);
            }
        }
    }

    /// Fills in the sidebar's unloaded databases and, with
    /// [`GotoScope::AllSaved`], remembers the names for Ctrl+K.
    fn collections_indexed(&mut self, connection: Uuid, databases: Vec<KnownDatabase>) {
        if let Some(entry) = self.entry_mut(connection) {
            for known in &databases {
                if let Some(node) = entry.database_mut(&known.database)
                    && matches!(node.collections, Collections::NotLoaded)
                {
                    node.collections = Collections::Loaded(known.collections.clone());
                }
            }
        }
        if self.settings.goto_scope == GotoScope::AllSaved {
            self.known.retain(|known| known.connection != connection);
            self.known.extend(databases);
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
        if let Some(dialog) = &mut self.transfer_dialog
            && dialog.running.as_ref().is_some_and(|(r, _)| *r == request)
        {
            dialog.finished(Err(BackendError::new(
                "Cancelled. Documents imported so far were kept.",
            )));
        }
        for tab in &mut self.tabs {
            if tab.pending_find == Some(request) {
                tab.pending_find = None;
            }
            if tab.pending_count == Some(request) {
                tab.pending_count = None;
            }
            if let Some(page) = tab.shell.result_for_request(request) {
                page.pending = None;
            }
        }
        if let Some(index) = self.running_tab(request) {
            self.finish_script(index, Some(Ending::Stopped));
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
            Action::EditMappings {
                connection,
                database,
            } => {
                let not_loaded = self
                    .entry(connection)
                    .and_then(|entry| entry.database(&database))
                    .is_some_and(|node| matches!(node.collections, Collections::NotLoaded));
                if not_loaded {
                    self.apply(Action::LoadCollections {
                        connection,
                        database: database.clone(),
                    });
                }
                let mut sources: Vec<Source> = self
                    .mappings
                    .iter()
                    .filter(|other| !(other.connection == connection && other.database == database))
                    .filter_map(|other| {
                        Some(Source {
                            label: self.location(other.connection, &other.database)?,
                            mappings: other.mappings.clone(),
                        })
                    })
                    .collect();
                sources.sort_by(|a, b| a.label.cmp(&b.label));
                let location = self.location(connection, &database).unwrap_or_default();
                let mappings = self.database_mappings(connection, &database).to_vec();
                self.mappings_dialog = Some(MappingsDialog::list(
                    connection, database, location, &mappings, sources,
                ));
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

    fn load_indexes(&mut self, namespace: &nordpil_backend::Namespace) {
        let request = self.backend.send(Command::ListIndexes(namespace.clone()));
        for tab in &mut self.tabs {
            if tab.namespace() == *namespace {
                tab.indexes_request = Some(request);
            }
        }
    }

    fn index_action(&mut self, index: usize, action: IndexAction) {
        let namespace = self.tabs[index].namespace();
        match action {
            IndexAction::Refresh => self.load_indexes(&namespace),
            IndexAction::Create => self.index_dialog = Some(IndexDialog::create(namespace)),
            IndexAction::Drop(name) => self.index_dialog = Some(IndexDialog::drop(namespace, name)),
            IndexAction::SetHidden { name, hidden } => {
                self.backend.send(Command::SetIndexHidden {
                    namespace,
                    name,
                    hidden,
                });
            }
        }
    }

    fn show_transfer_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.transfer_dialog else {
            return;
        };
        match dialog.show(ctx) {
            transfer_dialog::Outcome::Open => {}
            transfer_dialog::Outcome::Close => self.transfer_dialog = None,
            transfer_dialog::Outcome::Send(command, progress) => {
                let request = self.backend.send(*command);
                dialog.started(request, progress);
            }
            transfer_dialog::Outcome::Cancel(request) => self.backend.cancel(request),
        }
    }

    fn show_index_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.index_dialog else {
            return;
        };
        let fields = self
            .tabs
            .iter()
            .find(|tab| tab.namespace() == dialog.namespace)
            .map(|tab| tab.fields.clone())
            .unwrap_or_default();
        match dialog.show(ctx, &fields) {
            index_dialog::Outcome::Open => {}
            index_dialog::Outcome::Cancel => self.index_dialog = None,
            index_dialog::Outcome::Send(command) => {
                dialog.pending = Some(self.backend.send(*command));
            }
        }
    }

    fn show_document_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.document_dialog else {
            return;
        };
        let fields = self
            .tabs
            .iter()
            .find(|tab| tab.namespace() == dialog.namespace)
            .map(|tab| tab.fields.clone())
            .unwrap_or_default();
        match dialog.show(ctx, &fields) {
            document_dialogs::Outcome::Open => {}
            document_dialogs::Outcome::Cancel => self.document_dialog = None,
            document_dialogs::Outcome::Send(command) => {
                dialog.pending = Some(self.backend.send(*command));
            }
        }
    }

    fn show_update(&mut self, ui: &mut egui::Ui) {
        let Some(updater) = &mut self.updater else {
            return;
        };
        if !updater.visible() {
            return;
        }
        let palette = theme::palette(ui);
        let (message, release, error) = match updater.state() {
            update::State::Available(release) => (
                format!("nordpil {} is available.", release.version),
                release,
                None,
            ),
            update::State::Installing(release) => (
                format!("Installing nordpil {}…", release.version),
                release,
                None,
            ),
            update::State::Installed(release) => (
                format!(
                    "nordpil {} is installed. Restart to use it.",
                    release.version
                ),
                release,
                None,
            ),
            update::State::Failed(release, error) => (
                format!("Could not update to nordpil {}.", release.version),
                release,
                Some(error.clone()),
            ),
            update::State::Idle | update::State::Checking => return,
        };
        let page = release.page.clone();
        let installable = updater.can_install() && release.download.is_some();
        let state = updater.state().clone();
        let mut clicked = None;
        egui::Panel::bottom("update")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(palette.main)
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                Frame::new()
                    .fill(palette.accent_soft)
                    .stroke(Stroke::new(1.0, palette.accent.gamma_multiply(0.35)))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if matches!(state, update::State::Installing(_)) {
                                ui.spinner();
                            } else {
                                ui.label(
                                    RichText::new(icons::DOWNLOAD)
                                        .size(16.0)
                                        .color(palette.accent_text),
                                );
                            }
                            let label = ui.label(RichText::new(&message).color(palette.text));
                            if let Some(error) = &error {
                                label.on_hover_text(error);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !matches!(state, update::State::Installing(_))
                                        && kit::icon_button(ui, icons::X, "Dismiss", 26.0).clicked()
                                    {
                                        clicked = Some(UpdateClick::Dismiss);
                                    }
                                    let height = 28.0;
                                    let primary = match &state {
                                        update::State::Available(_) if installable => {
                                            Some(("Install", UpdateClick::Install))
                                        }
                                        update::State::Installed(_) => {
                                            Some(("Restart", UpdateClick::Restart))
                                        }
                                        update::State::Failed(..) if installable => {
                                            Some(("Retry", UpdateClick::Install))
                                        }
                                        _ => None,
                                    };
                                    if let Some((label, click)) = primary
                                        && kit::button(ui, Tone::Primary, None, label, height)
                                            .clicked()
                                    {
                                        clicked = Some(click);
                                    }
                                    let link = if installable {
                                        "Release notes"
                                    } else {
                                        "Download"
                                    };
                                    if kit::button(ui, Tone::Ghost, None, link, height).clicked() {
                                        ui.ctx().open_url(egui::OpenUrl::new_tab(&page));
                                    }
                                },
                            );
                        });
                    });
            });
        match clicked {
            Some(UpdateClick::Install) => updater.install(),
            Some(UpdateClick::Restart) => updater.restart(),
            Some(UpdateClick::Dismiss) => updater.dismiss(),
            None => {}
        }
    }

    fn show_notifications(&mut self, ui: &mut egui::Ui) {
        if self.notifications.is_empty() {
            return;
        }
        let palette = theme::palette(ui);
        egui::Panel::bottom("notifications")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(palette.main)
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                let mut dismissed = None;
                for (index, error) in self.notifications.iter().enumerate() {
                    Frame::new()
                        .fill(palette.danger_soft)
                        .stroke(Stroke::new(1.0, palette.danger.gamma_multiply(0.35)))
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(icons::CIRCLE_ALERT)
                                        .size(16.0)
                                        .color(palette.danger),
                                );
                                let label =
                                    ui.label(RichText::new(&error.message).color(palette.text));
                                if let Some(detail) = &error.detail {
                                    label.on_hover_text(detail);
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if kit::icon_button(ui, icons::X, "Dismiss", 26.0).clicked()
                                        {
                                            dismissed = Some(index);
                                        }
                                    },
                                );
                            });
                        });
                }
                if let Some(index) = dismissed {
                    self.notifications.remove(index);
                }
            });
    }

    fn show_main(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let palette = theme::palette(ui);
        if self.tabs.is_empty() {
            if chrome::welcome(ui, !self.connections.is_empty()) {
                actions.push(Action::NewConnection);
            }
            return;
        }

        let titles: Vec<chrome::TabTitle> = self
            .tabs
            .iter()
            .map(|tab| chrome::TabTitle {
                database: tab.target.database.clone(),
                collection: tab.target.collection.clone(),
                color: self
                    .entry(tab.target.connection)
                    .and_then(|entry| entry.saved.color)
                    .map(sidebar::color32),
            })
            .collect();
        let bar = ui
            .scope(|ui| {
                ui.set_height(chrome::TAB_BAR_HEIGHT);
                let rect = ui.max_rect();
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.right(), rect.top() + chrome::TAB_BAR_HEIGHT),
                    ),
                    0.0,
                    palette.side,
                );
                ui.painter().hline(
                    rect.x_range(),
                    rect.top() + chrome::TAB_BAR_HEIGHT - 0.5,
                    Stroke::new(1.0, palette.line),
                );
                chrome::tab_bar(ui, &titles, self.active_tab)
            })
            .inner;
        match bar {
            Some(chrome::TabEvent::Select(index)) => self.active_tab = Some(index),
            Some(chrome::TabEvent::Close(index)) => {
                self.close_tab(index);
                return;
            }
            None => {}
        }

        let Some(index) = self.active_tab else {
            return;
        };
        let entry = self.entry(self.tabs[index].target.connection);
        let read_only = entry.is_none_or(|entry| entry.saved.read_only);
        let connection_name = entry
            .map(|entry| entry.saved.name.clone())
            .unwrap_or_default();
        let count = match self.tabs[index].count {
            Some(Some(count)) => Some(format!("{} documents", documents::thousands(count))),
            _ => None,
        };
        Frame::new()
            .inner_margin(Margin {
                left: 24,
                right: 24,
                top: 18,
                bottom: 0,
            })
            .show(ui, |ui| {
                let tab = &mut self.tabs[index];
                let target = tab.target.clone();
                chrome::collection_header(
                    ui,
                    &connection_name,
                    &target.database,
                    &target.collection,
                    count,
                    &mut tab.panel,
                );
                ui.add_space(14.0);
                match self.tabs[index].panel {
                    Panel::Documents => {
                        let references = self.references(index);
                        if let Some(action) = documents::show(
                            ui,
                            &mut self.tabs[index],
                            read_only,
                            references.as_ref(),
                        ) {
                            self.tab_action(index, action);
                        }
                    }
                    Panel::Indexes => {
                        let tab = &self.tabs[index];
                        if tab.indexes.is_none() && tab.indexes_request.is_none() {
                            let namespace = tab.namespace();
                            self.load_indexes(&namespace);
                        }
                        if let Some(action) = indexes::show(ui, &self.tabs[index], read_only) {
                            self.index_action(index, action);
                        }
                    }
                    Panel::Shell => {
                        let target = self.tabs[index].target.clone();
                        self.ensure_collections(&target);
                        let collections =
                            self.collection_names(target.connection, &target.database);
                        // Lookups go to the shown result's database.
                        let database = self.tabs[index]
                            .shell
                            .shown_database()
                            .unwrap_or(&target.database)
                            .to_owned();
                        let shown = Selection {
                            database,
                            ..target.clone()
                        };
                        self.load_references(&shown);
                        let references = self.references_in(shown.connection, &shown.database);
                        let surroundings = crate::ui::shell::Surroundings {
                            collections: &collections,
                            layout: &mut self.settings.shell,
                            references: references.as_ref(),
                        };
                        if let Some(action) =
                            crate::ui::shell::show(ui, &mut self.tabs[index], surroundings)
                        {
                            self.shell_action(ui.ctx(), index, action);
                        }
                    }
                }
            });
    }

    /// App-wide keys; see [`shortcuts::BINDINGS`].
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let mut commands = ctx.input_mut(shortcuts::pressed);
        #[cfg(target_os = "macos")]
        if let Some(menu) = &mut self.menu {
            commands.extend(menu.commands());
            let on_shell = self
                .active_tab
                .is_some_and(|index| self.tabs[index].panel == Panel::Shell);
            menu.update(crate::menu::MenuState {
                has_tabs: !self.tabs.is_empty(),
                can_reopen: !self.closed.is_empty(),
                on_shell,
            });
        }
        if self.escape_stops_script(ctx)
            && ctx.input_mut(|input| input.consume_shortcut(&shortcuts::ESCAPE))
        {
            commands.push(AppCommand::StopScript);
        }
        for command in commands {
            self.run_command(ctx, command);
        }
    }

    /// Escape stops the active tab's script unless a popup or dialog would
    /// close on it instead.
    fn escape_stops_script(&self, ctx: &egui::Context) -> bool {
        let Some(tab) = self.active_tab.map(|index| &self.tabs[index]) else {
            return false;
        };
        tab.panel == Panel::Shell
            && tab.shell.running.is_some()
            && !tab.shell.editor.popup_open()
            && !egui::Popup::is_any_open(ctx)
            && self.open_modal().is_none()
    }

    /// Which modal is open. App-wide keys wait while one is, except the key
    /// that toggles that same modal.
    fn open_modal(&self) -> Option<Modal> {
        let write_waits = self.tabs.iter().any(|tab| {
            tab.shell
                .running
                .as_ref()
                .is_some_and(|running| running.run.pending_write().is_some())
        });
        if self.form.is_some()
            || self.dialog.is_some()
            || self.document_dialog.is_some()
            || self.index_dialog.is_some()
            || self.transfer_dialog.is_some()
            || self.mappings_dialog.is_some()
            || write_waits
        {
            Some(Modal::Other)
        } else if self.settings_dialog.is_some() {
            Some(Modal::Settings)
        } else if self.goto.is_some() {
            Some(Modal::GoTo)
        } else if self.shortcuts_sheet {
            Some(Modal::Shortcuts)
        } else {
            None
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, command: AppCommand) {
        let toggles_own_modal = matches!(
            (self.open_modal(), command),
            (Some(Modal::GoTo), AppCommand::GoTo)
                | (Some(Modal::Settings), AppCommand::Settings)
                | (Some(Modal::Shortcuts), AppCommand::Shortcuts)
        );
        if self.open_modal().is_some() && !toggles_own_modal {
            return;
        }
        match command {
            AppCommand::GoTo => {
                self.goto = match self.goto {
                    Some(_) => None,
                    None => Some(GotoPalette::new()),
                };
            }
            AppCommand::FocusEditor => {
                let Some(index) = self.active_tab else {
                    return;
                };
                let tab = &mut self.tabs[index];
                if tab.panel == Panel::Shell {
                    tab.shell.editor.focus(ctx, &tab.shell.text);
                } else {
                    tab.panel = Panel::Documents;
                    tab.editors
                        .filter
                        .focus_and_select_all(ctx, &tab.inputs.filter);
                }
            }
            AppCommand::CloseTab => {
                if let Some(index) = self.active_tab {
                    self.close_tab(index);
                }
            }
            AppCommand::ReopenTab => self.reopen_tab(),
            AppCommand::NextTab | AppCommand::PreviousTab => {
                let count = self.tabs.len();
                if let Some(index) = self.active_tab {
                    let step = if command == AppCommand::NextTab {
                        1
                    } else {
                        count - 1
                    };
                    self.active_tab = Some((index + step) % count);
                }
            }
            AppCommand::SelectTab(index) => {
                if index < self.tabs.len() {
                    self.active_tab = Some(index);
                }
            }
            AppCommand::LastTab => self.active_tab = self.tabs.len().checked_sub(1),
            AppCommand::Panel(panel) => {
                if let Some(index) = self.active_tab {
                    self.tabs[index].panel = panel;
                }
            }
            AppCommand::Refresh => {
                if let Some(index) = self.active_tab {
                    self.refresh(index);
                }
            }
            AppCommand::Settings => {
                self.settings_dialog = match self.settings_dialog {
                    Some(_) => None,
                    None => Some(SettingsDialog),
                };
            }
            AppCommand::Shortcuts => self.shortcuts_sheet = !self.shortcuts_sheet,
            AppCommand::SaveScript | AppCommand::OpenScript | AppCommand::StopScript => {
                let Some(index) = self.active_tab else {
                    return;
                };
                if let Some(action) = script_action(self.tabs[index].panel, command) {
                    self.shell_action(ctx, index, action);
                }
            }
        }
    }

    fn show_goto(&mut self, ctx: &egui::Context) {
        if self.goto.is_none() {
            return;
        }
        let candidates = self.goto_candidates();
        let searches_closed = self.settings.goto_scope == GotoScope::AllSaved;
        let current = self.selection().cloned();
        let Some(goto) = self.goto.as_mut() else {
            return;
        };
        match goto.show(
            ctx,
            &candidates,
            &self.recents,
            current.as_ref(),
            searches_closed,
        ) {
            goto::Outcome::Open => {}
            goto::Outcome::Close => self.goto = None,
            goto::Outcome::Pick(target) => {
                self.goto = None;
                self.go_to(target);
            }
        }
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.settings_dialog else {
            return;
        };
        let mut settings = self.settings;
        match dialog.show(ctx, &mut settings) {
            settings_dialog::Outcome::Open => {}
            settings_dialog::Outcome::Close => self.settings_dialog = None,
            settings_dialog::Outcome::Changed => self.set_settings(ctx, settings),
        }
    }

    /// Applies the backend results that arrived since the last frame.
    pub fn process_events(&mut self) {
        if let Some(updater) = &mut self.updater {
            updater.poll(self.settings.updates);
        }
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
        let ctx = ui.ctx().clone();
        self.poll_shells(&ctx);
        self.shortcuts(&ctx);
        let palette = theme::palette(ui);

        let top = egui::Panel::top("top_bar")
            .exact_size(chrome::TOP_BAR_HEIGHT)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(palette.app)
                    .inner_margin(Margin::symmetric(10, 0)),
            )
            .show(ui, chrome::top_bar);
        let rect = top.response.rect;
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, palette.line),
        );
        if top.inner.goto && self.goto.is_none() {
            self.goto = Some(GotoPalette::new());
        }
        if top.inner.settings {
            self.settings_dialog = Some(SettingsDialog);
        }

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(272.0)
            .size_range(200.0..=480.0)
            .show_separator_line(false)
            .frame(Frame::new().fill(palette.side))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().vline(
                    rect.right() + 0.5,
                    rect.y_range(),
                    Stroke::new(1.0, palette.line),
                );
                sidebar::show(
                    ui,
                    &self.connections,
                    self.selection(),
                    self.settings.look_up_references,
                    &mut actions,
                );
            });
        self.show_update(ui);
        self.show_notifications(ui);
        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(palette.main))
            .show(ui, |ui| self.show_main(ui, &mut actions));

        self.show_form(&ctx);
        self.show_document_dialog(&ctx);
        self.show_index_dialog(&ctx);
        self.show_transfer_dialog(&ctx);
        if let Some(dialog) = &mut self.dialog
            && !dialog.show(&ctx, &mut actions)
        {
            // A password prompt closed without connecting cancels Ctrl+K's pick.
            let cancelled_password = matches!(dialog, Dialog::Password(_))
                && !actions
                    .iter()
                    .any(|action| matches!(action, Action::ConnectWithPassword { .. }));
            if cancelled_password {
                self.pending_open = None;
            }
            self.dialog = None;
        }
        self.show_settings(&ctx);
        if self.shortcuts_sheet {
            self.shortcuts_sheet = shortcuts_dialog::show(&ctx);
        }
        self.show_mappings_dialog(&ctx);
        self.show_goto(&ctx);
        self.show_write_confirmation(&ctx);

        for action in actions {
            self.apply(action);
        }
        self.remember_panel();
    }

    /// Keeps the active tab's panel in the settings, for new tabs to open on.
    fn remember_panel(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        let panel = self.tabs[index].panel;
        if panel != self.settings.last_panel {
            self.settings.last_panel = panel;
            self.backend.send(Command::SaveSettings(self.settings));
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
            .with_context_setup(theme::install)
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

    /// An app with lookups on, showing `document` in `shop.orders` of a
    /// connected connection whose `shop` has `collections`.
    fn lookup_harness(
        collections: &[&str],
        document: bson::Document,
    ) -> Harness<'static, NordpilApp> {
        use nordpil_backend::{CollectionInfo, CollectionKind};

        use crate::connections::DatabaseNode;

        let mut harness = harness();
        // Let the initial (empty) load finish so it doesn't replace the test data.
        run_until(&mut harness, |app| app.keyring_available);
        let app = harness.state_mut();
        let mut entry = ConnectionEntry::new(SavedConnection {
            id: Uuid::new_v4(),
            name: "Local".to_owned(),
            uri: "mongodb://localhost:27017".to_owned(),
            color: None,
            read_only: false,
        });
        entry.state = ConnectionState::Connected(vec![DatabaseNode {
            info: DatabaseInfo {
                name: "shop".to_owned(),
                size_on_disk: 0,
                empty: false,
            },
            collections: Collections::Loaded(
                collections
                    .iter()
                    .map(|name| CollectionInfo {
                        name: (*name).to_owned(),
                        kind: CollectionKind::Collection,
                        capped: false,
                    })
                    .collect(),
            ),
        }]);
        let target = Selection {
            connection: entry.saved.id,
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
        };
        app.connections = vec![entry];
        app.settings.look_up_references = true;
        let mut tab = CollectionTab::new(1, target);
        tab.set_documents(vec![document]);
        app.tabs.push(tab);
        app.active_tab = Some(0);
        app.next_tab_id = 1;
        harness.run();
        harness
    }

    #[test]
    fn lookup_opens_the_referenced_document_in_a_new_tab() {
        let customer = bson::oid::ObjectId::from_bytes([7; 12]);
        let mut harness = lookup_harness(
            &["customers", "orders"],
            bson::doc! { "_id": 1, "customerId": customer },
        );

        harness.get_by_label("customerId:").click_secondary();
        harness.run();
        harness.get_by_label("Open in customers").click();
        harness.run();

        let app = harness.state();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, Some(1));
        assert_eq!(app.tabs[1].target.collection, "customers");
        assert_eq!(
            app.tabs[1].inputs.filter,
            format!("{{ _id: ObjectId('{}') }}", customer.to_hex())
        );
    }

    /// `lookup_harness` with a second tab, on "customers", active.
    fn two_tabs_harness() -> Harness<'static, NordpilApp> {
        let mut harness = lookup_harness(&["customers", "orders"], bson::doc! { "_id": 1 });
        let target = Selection {
            collection: "customers".to_owned(),
            ..harness.state().tabs[0].target.clone()
        };
        harness.state_mut().open_tab(target);
        harness.run();
        harness
    }

    #[test]
    fn ctrl_l_focuses_the_shell_script_where_its_cursor_was() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let tab = &mut harness.state_mut().tabs[0];
        tab.panel = Panel::Shell;
        tab.shell.load_draft(None, "orders");
        harness.run();
        let ctx = harness.ctx.clone();
        let editor = harness.state().tabs[0].shell.editor.id();
        harness.state().tabs[0].shell.editor.focus_at(&ctx, 3);
        harness.run();
        ctx.memory_mut(|memory| memory.surrender_focus(editor));
        harness.run();
        assert!(!ctx.memory(|memory| memory.has_focus(editor)));

        harness.key_press_modifiers(Modifiers::COMMAND, Key::L);
        harness.run();

        let tab = &harness.state().tabs[0];
        assert_eq!(tab.panel, Panel::Shell);
        assert!(ctx.memory(|memory| memory.has_focus(editor)));
        assert_eq!(
            tab.shell.editor.selection(&ctx, &tab.shell.text),
            Some(3..3)
        );
    }

    #[test]
    fn ctrl_l_off_the_shell_selects_the_whole_filter() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let tab = &mut harness.state_mut().tabs[0];
        tab.panel = Panel::Indexes;
        tab.inputs.filter = "{ total: 5 }".to_owned();
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::L);
        harness.run();

        let ctx = harness.ctx.clone();
        let tab = &harness.state().tabs[0];
        assert_eq!(tab.panel, Panel::Documents);
        assert!(ctx.memory(|memory| memory.has_focus(tab.editors.filter.id())));
        assert_eq!(
            tab.editors.filter.selection(&ctx, &tab.inputs.filter),
            Some(0..tab.inputs.filter.len())
        );
    }

    #[test]
    fn ctrl_w_closes_the_active_tab_and_then_does_nothing() {
        let mut harness = two_tabs_harness();
        assert_eq!(harness.state().active_tab, Some(1));

        harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
        harness.run();
        let app = harness.state();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].target.collection, "orders");
        assert_eq!(app.active_tab, Some(0));

        harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
        harness.run();
        assert!(harness.state().tabs.is_empty());
        assert_eq!(harness.state().active_tab, None);
    }

    #[test]
    fn ctrl_comma_opens_and_closes_settings() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        assert!(harness.state().settings_dialog.is_some());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        assert!(harness.state().settings_dialog.is_none());
    }

    /// Saves and connects to the MongoDB on `port`, then opens a Shell tab.
    fn connected_shell_harness(port: u16) -> Harness<'static, NordpilApp> {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "New connection")
            .click();
        harness.run();
        let uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true");
        type_into(&mut harness, "Connection string", &uri);
        harness
            .get_by_role_and_label(Role::Button, "Save and connect")
            .click();
        run_until(&mut harness, |app| {
            matches!(
                app.connections.first().map(|e| &e.state),
                Some(ConnectionState::Connected(_))
            )
        });
        let target = Selection {
            connection: harness.state().connections[0].saved.id,
            database: "test".to_owned(),
            collection: "items".to_owned(),
        };
        harness.state_mut().open_tab(target);
        harness.state_mut().tabs[0].panel = Panel::Shell;
        harness.run();
        harness
    }

    #[test]
    #[ignore = "needs Docker"]
    fn ctrl_period_and_escape_stop_a_running_script() {
        let mongo = Mongo::default().with_tag("8.0").start().unwrap();
        let port = mongo.get_host_port_ipv4(27017).unwrap();
        let mut harness = connected_shell_harness(port);
        let stopped = |app: &NordpilApp| {
            app.tabs[0].shell.running.is_none()
                && matches!(app.tabs[0].shell.ending, Some(Ending::Stopped))
        };

        harness
            .state_mut()
            .run_script(0, "while (true) {}".to_owned());
        // The spinner keeps repainting while it runs.
        harness.step();
        assert!(harness.state().tabs[0].shell.running.is_some());
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Period);
        run_until(&mut harness, stopped);

        harness
            .state_mut()
            .run_script(0, "while (true) { 1 }".to_owned());
        // The spinner keeps repainting while it runs.
        harness.step();
        assert!(harness.state().tabs[0].shell.running.is_some());
        harness.key_press(Key::Escape);
        run_until(&mut harness, stopped);
    }

    #[test]
    fn script_keys_only_work_on_the_shell_panel() {
        // Pressing them on the Shell panel would open a real file dialog.
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        harness.key_press_modifiers(Modifiers::COMMAND, Key::O);
        harness.run();
        assert!(harness.state().tabs[0].shell.picker.is_none());

        let on = |panel, command| script_action(panel, command);
        assert!(matches!(
            on(Panel::Shell, AppCommand::OpenScript),
            Some(ShellAction::File(FileJob::Open))
        ));
        assert!(matches!(
            on(Panel::Shell, AppCommand::SaveScript),
            Some(ShellAction::File(FileJob::Save))
        ));
        assert!(matches!(
            on(Panel::Shell, AppCommand::StopScript),
            Some(ShellAction::Stop)
        ));
        assert!(on(Panel::Documents, AppCommand::SaveScript).is_none());
        assert!(on(Panel::Indexes, AppCommand::StopScript).is_none());
    }

    #[test]
    fn ctrl_tab_cycles_tabs_and_ctrl_digits_jump_to_one() {
        let mut harness = two_tabs_harness();
        let target = Selection {
            collection: "invoices".to_owned(),
            ..harness.state().tabs[0].target.clone()
        };
        harness.state_mut().open_tab(target);
        harness.run();
        let mut press = |modifiers, key| {
            harness.key_press_modifiers(modifiers, key);
            harness.run();
            harness.state().active_tab
        };

        assert_eq!(press(Modifiers::CTRL, Key::Tab), Some(0));
        assert_eq!(press(Modifiers::CTRL, Key::Tab), Some(1));
        assert_eq!(press(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab), Some(0));
        assert_eq!(press(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab), Some(2));
        assert_eq!(press(Modifiers::COMMAND, Key::Num2), Some(1));
        assert_eq!(press(Modifiers::COMMAND, Key::Num9), Some(2));
        assert_eq!(press(Modifiers::COMMAND, Key::Num1), Some(0));
        assert_eq!(press(Modifiers::COMMAND, Key::Num5), Some(0));
    }

    #[test]
    fn ctrl_shift_t_brings_back_a_closed_tab_as_it_was() {
        let mut harness = two_tabs_harness();
        let tab = &mut harness.state_mut().tabs[1];
        tab.inputs.filter = "{ vip: true }".to_owned();
        tab.inputs.limit = "5".to_owned();
        tab.show_options = true;
        tab.view = View::Table;
        tab.page_size = 50;
        tab.panel = Panel::Indexes;
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
        harness.run();
        assert_eq!(harness.state().tabs.len(), 1);
        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
        harness.run();

        let app = harness.state();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, Some(1));
        let tab = &app.tabs[1];
        assert_eq!(tab.target.collection, "customers");
        assert_eq!(tab.inputs.filter, "{ vip: true }");
        assert_eq!(tab.inputs.limit, "5");
        assert!(tab.show_options);
        assert_eq!(tab.view, View::Table);
        assert_eq!(tab.page_size, 50);
        assert_eq!(tab.panel, Panel::Indexes);
    }

    #[test]
    fn closed_tabs_keep_the_last_ten_closed_by_the_user() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let app = harness.state_mut();
        let target = app.tabs[0].target.clone();
        for _ in 0..12 {
            app.push_tab(target.clone(), String::new());
            app.close_tab(app.tabs.len() - 1);
        }
        assert_eq!(app.closed.len(), MAX_CLOSED);

        // Closing for a drop or a disconnect doesn't count.
        app.deselect_if(|selection| selection.collection == "orders");
        assert!(app.tabs.is_empty());
        assert_eq!(app.closed.len(), MAX_CLOSED);

        // A closed tab of a removed connection is skipped.
        let mut gone = ClosedTab::of(&CollectionTab::new(99, target.clone()));
        gone.target.connection = Uuid::new_v4();
        app.closed.push(gone);
        app.reopen_tab();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].target, target);
        assert_eq!(app.closed.len(), MAX_CLOSED - 1);
    }

    #[test]
    fn alt_digits_switch_the_panel() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let mut press = |key| {
            harness.key_press_modifiers(Modifiers::ALT, key);
            harness.run();
            harness.state().tabs[0].panel
        };

        assert_eq!(press(Key::Num3), Panel::Shell);
        assert_eq!(press(Key::Num2), Panel::Indexes);
        assert_eq!(press(Key::Num1), Panel::Documents);
    }

    #[test]
    fn ctrl_r_reruns_on_the_same_page_unless_the_inputs_changed() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let tab = &mut harness.state_mut().tabs[0];
        tab.applied = Some(build_query(&tab.inputs).unwrap());
        tab.page = 2;
        tab.count = Some(Some(70));
        harness.run();

        // The test backend has no server, so the count comes back failed.
        harness.key_press_modifiers(Modifiers::COMMAND, Key::R);
        harness.run();
        let tab = &harness.state().tabs[0];
        assert_eq!(tab.page, 2);
        assert_ne!(tab.count, Some(Some(70)));

        harness.state_mut().tabs[0].inputs.filter = "{ total: 5 }".to_owned();
        harness.key_press(Key::F5);
        harness.run();
        let tab = &harness.state().tabs[0];
        assert_eq!(tab.page, 0);
        let expected = build_query(&tab.inputs).unwrap();
        assert_eq!(tab.applied.as_ref(), Some(&expected));
    }

    #[test]
    fn ctrl_r_reloads_indexes_and_leaves_the_shell_alone() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });
        let tab = &mut harness.state_mut().tabs[0];
        tab.panel = Panel::Shell;
        tab.count = Some(Some(70));
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::R);
        harness.run();
        let tab = &harness.state().tabs[0];
        assert_eq!(tab.count, Some(Some(70)));
        assert!(tab.shell.running.is_none());

        let tab = &mut harness.state_mut().tabs[0];
        tab.panel = Panel::Indexes;
        tab.indexes = Some(Ok(Vec::new()));
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::R);
        // Without a server the reload fails, which shows it was asked for.
        run_until(&mut harness, |app| {
            app.tabs[0].indexes_request.is_some() || matches!(app.tabs[0].indexes, Some(Err(_)))
        });
    }

    #[test]
    fn f1_and_ctrl_slash_toggle_the_shortcuts_sheet() {
        let mut harness = lookup_harness(&["orders"], bson::doc! { "_id": 1 });

        harness.key_press(Key::F1);
        harness.run();
        assert!(harness.state().shortcuts_sheet);
        assert!(harness.query_by_label("Reopen closed tab").is_some());
        harness.key_press(Key::F1);
        harness.run();
        assert!(!harness.state().shortcuts_sheet);

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Slash);
        harness.run();
        assert!(harness.state().shortcuts_sheet);
        harness.key_press(Key::Escape);
        harness.run();
        assert!(!harness.state().shortcuts_sheet);
    }

    #[test]
    fn shortcuts_wait_while_a_modal_is_open() {
        let mut harness = two_tabs_harness();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        assert!(harness.state().settings_dialog.is_some());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::W);
        harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
        harness.key_press(Key::F1);
        harness.key_press_modifiers(Modifiers::CTRL, Key::Tab);
        harness.run();
        let app = harness.state();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, Some(1));
        assert!(app.goto.is_none());
        assert!(!app.shortcuts_sheet);

        // The key that opened it still closes it.
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        assert!(harness.state().settings_dialog.is_none());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
        harness.run();
        assert!(harness.state().goto.is_some());
        assert!(harness.state().settings_dialog.is_none());
    }

    #[test]
    fn new_tabs_open_on_the_last_used_panel() {
        let mut harness = lookup_harness(&["customers", "orders"], bson::doc! { "_id": 1 });
        harness.state_mut().tabs[0].panel = Panel::Shell;
        harness.run();
        assert_eq!(harness.state().settings.last_panel, Panel::Shell);

        let target = Selection {
            collection: "customers".to_owned(),
            ..harness.state().tabs[0].target.clone()
        };
        harness.state_mut().open_tab(target);
        harness.run();

        let app = harness.state();
        assert_eq!(app.active_tab, Some(1));
        assert_eq!(app.tabs[1].panel, Panel::Shell);
        assert_eq!(app.tabs[0].panel, Panel::Shell);
    }

    #[test]
    fn lookup_from_the_shell_appends_and_runs_a_find() {
        use crate::shell::{Body, Selected, ShellResult};

        let customer = bson::oid::ObjectId::from_bytes([7; 12]);
        let mut harness = lookup_harness(&["customers", "orders"], bson::doc! { "_id": 1 });
        {
            let tab = &mut harness.state_mut().tabs[0];
            tab.panel = Panel::Shell;
            tab.shell.load_draft(None, "orders");
            let documents = vec![bson::doc! { "_id": 1, "customerId": customer }];
            tab.shell.results.push(ShellResult {
                title: "db.orders.find({}).toArray()".to_owned(),
                elapsed: Duration::from_millis(3),
                body: Body::documents(documents, 20),
            });
            tab.shell.selected = Some(Selected::Result(0));
        }
        harness.run();

        harness.get_by_label("customerId:").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Add to filter").is_none());
        harness.get_by_label("Open in customers").click();
        harness.run();
        run_until(&mut harness, |app| {
            app.tabs.get(1).is_some_and(|tab| tab.shell.loaded)
        });

        let app = harness.state();
        let tab = &app.tabs[1];
        let find = format!(
            "db.customers.find({{ _id: ObjectId('{}') }})",
            customer.to_hex()
        );
        assert_eq!(app.active_tab, Some(1));
        assert_eq!(tab.panel, Panel::Shell);
        assert_eq!(tab.target.collection, "customers");
        assert_eq!(tab.shell.text, find);
        assert_eq!(tab.shell.script, find);
    }

    #[test]
    fn linking_a_field_offers_its_collection() {
        let mut harness = lookup_harness(
            &["accounts", "orders"],
            bson::doc! { "_id": 1, "ownerRef": "a-1" },
        );
        harness.get_by_label("ownerRef:").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Open in accounts").is_none());

        harness.get_by_label("Link to collection…").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::ComboBox, "Collection")
            .click();
        harness.run();
        harness.get_by_label("accounts").click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Save").click();
        harness.run();

        let app = harness.state();
        assert!(app.mappings_dialog.is_none());
        assert_eq!(
            app.database_mappings(app.tabs[0].target.connection, "shop"),
            [Mapping {
                key: "ownerRef".to_owned(),
                collection: "accounts".to_owned(),
                field: None,
            }]
        );
        harness.get_by_label("ownerRef:").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Open in accounts").is_some());
        assert!(harness.query_by_label("Edit link…").is_some());
    }

    /// Runs `script` in mongosh inside the container and returns what it printed.
    fn mongosh(
        mongo: &testcontainers_modules::testcontainers::Container<Mongo>,
        script: &str,
    ) -> String {
        use testcontainers_modules::testcontainers::core::ExecCommand;

        let mut result = mongo
            .exec(ExecCommand::new(["mongosh", "--quiet", "--eval", script]))
            .unwrap();
        String::from_utf8(result.stdout_to_vec().unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }

    /// How many operations on `shop.slow` the server is running.
    fn running_on_server(mongo: &testcontainers_modules::testcontainers::Container<Mongo>) -> u32 {
        mongosh(
            mongo,
            "db.getSiblingDB('admin').aggregate([{ $currentOp: {} }, { $match: { ns: 'shop.slow' } }]).toArray().length",
        )
        .parse()
        .unwrap()
    }

    #[test]
    #[ignore = "needs Docker"]
    fn stop_ends_a_slow_query_on_the_server() {
        let mongo = Mongo::default().with_tag("8.0").start().unwrap();
        let port = mongo.get_host_port_ipv4(27017).unwrap();
        mongosh(
            &mongo,
            "db.getSiblingDB('shop').slow.insertMany(Array.from({ length: 2000 }, (_, n) => ({ n })))",
        );
        let mut harness = harness();
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "New connection")
            .click();
        harness.run();
        let uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true");
        type_into(&mut harness, "Connection string", &uri);
        harness
            .get_by_role_and_label(Role::Button, "Save and connect")
            .click();
        run_until(&mut harness, |app| {
            matches!(
                app.connections.first().map(|e| &e.state),
                Some(ConnectionState::Connected(_))
            )
        });
        let connection = harness.state().connections[0].saved.id;
        harness.state_mut().apply(Action::Select(Selection {
            connection,
            database: "shop".to_owned(),
            collection: "slow".to_owned(),
        }));
        run_until(&mut harness, |app| {
            app.tabs.first().is_some_and(|tab| tab.results.is_some())
        });

        // Matches nothing and sleeps 50 ms per document: over a minute of scanning.
        harness.state_mut().tabs[0].inputs.filter =
            "{ $where: 'function () { sleep(50); return false; }' }".to_owned();
        harness.state_mut().tab_action(0, TabAction::Run);
        let deadline = Instant::now() + Duration::from_secs(10);
        while running_on_server(&mongo) == 0 {
            assert!(Instant::now() < deadline, "the query never started");
            harness.step();
        }
        harness.step();
        assert!(harness.state().tabs[0].is_loading());

        harness.get_by_role_and_label(Role::Button, "Stop").click();
        harness.step();
        harness.step();
        assert!(!harness.state().tabs[0].is_loading());
        assert!(harness.query_by_label("Stopped.").is_some());
        let deadline = Instant::now() + Duration::from_secs(3);
        while running_on_server(&mongo) > 0 {
            assert!(
                Instant::now() < deadline,
                "the query still runs on the server"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    #[ignore = "needs Docker"]
    fn goto_connects_and_opens_a_remembered_collection() {
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
        harness
            .get_by_role_and_label(Role::Button, "Save and connect")
            .click();
        run_until(&mut harness, |app| {
            matches!(
                app.connections.first().map(|e| &e.state),
                Some(ConnectionState::Connected(_))
            )
        });
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
        // Connecting lists every collection, so Ctrl+K knows them once closed.
        harness.state_mut().apply(Action::Disconnect(connection));
        run_until(&mut harness, |app| {
            matches!(app.connections[0].state, ConnectionState::Disconnected)
        });
        harness.state_mut().connect(connection, None, false);
        run_until(&mut harness, |app| {
            app.known.iter().any(|known| known.database == "shop")
        });
        harness.state_mut().apply(Action::Disconnect(connection));
        run_until(&mut harness, |app| {
            matches!(app.connections[0].state, ConnectionState::Disconnected)
        });

        harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Go to collection")
            .type_text("ord");
        harness.run();
        harness.key_press(Key::Enter);
        run_until(&mut harness, |app| {
            app.tabs.first().is_some_and(|tab| tab.results.is_some())
        });

        let app = harness.state();
        assert!(app.goto.is_none());
        assert_eq!(app.tabs[0].target.collection, "orders");
        assert!(matches!(
            app.connections[0].state,
            ConnectionState::Connected(_)
        ));
        assert_eq!(app.recents[0].collection, "orders");
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
    use crate::ui::goto::GotoPalette;

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
        tab.fields = [
            "_id",
            "status",
            "total",
            "items",
            "items.sku",
            "items.qty",
            "customer.name",
            "customer.email",
            "created",
        ]
        .map(|path| nordpil_query::FieldHint {
            path: path.to_owned(),
            detail: String::new(),
        })
        .into();
        app.tabs.push(tab);
        app.active_tab = Some(0);

        let recent = |connection, database: &str, collection: &str| Recent {
            connection,
            database: database.to_owned(),
            collection: collection.to_owned(),
            opened_at: 0,
        };
        let (local, production) = (app.connections[0].saved.id, app.connections[1].saved.id);
        app.recents = vec![
            recent(local, "shop", "orders"),
            recent(local, "shop", "customers"),
            recent(local, "shop", "metrics"),
            recent(production, "shop", "orders"),
            recent(production, "auth", "users"),
        ];
        let known = |database: &str, names: &[&str]| KnownDatabase {
            connection: production,
            database: database.to_owned(),
            collections: names
                .iter()
                .map(|name| CollectionInfo {
                    name: (*name).to_owned(),
                    kind: CollectionKind::Collection,
                    capped: false,
                })
                .collect(),
        };
        app.known = vec![
            known("shop", &["orders", "users"]),
            known("analytics", &["order_events"]),
            known("archive", &["records"]),
        ];
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
            .with_size(egui::vec2(1440.0, 900.0))
            .with_context_setup(theme::install)
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
        harness
            .get_by_role_and_label(Role::TreeItem, "shop")
            .click();
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
            .get_by_role_and_label(Role::TextInput, "Filter")
            .focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "Filter")
            .type_text("{ status: 'shipped', cust");
        harness.run();
        save(&mut harness, "autocomplete");
        harness.key_press(egui::Key::Escape);
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "Filter")
            .type_text("{ status: '");
        harness.run();
        save(&mut harness, "autocomplete-values");
        harness.key_press(egui::Key::Escape);
        harness.run();
        harness.state_mut().tabs[0].view = crate::tabs::View::List;
        harness.state_mut().tabs[0].show_options = false;
        harness.state_mut().tabs[0].inputs.filter =
            "{ status: 'shipped', total: { $gt: 100 } }".to_owned();

        harness.state_mut().goto = Some(GotoPalette::new());
        harness.run();
        save(&mut harness, "goto-recent");
        harness
            .get_by_role_and_label(Role::TextInput, "Go to collection")
            .type_text("ord");
        harness.run();
        save(&mut harness, "goto-search");
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().goto.is_none());

        harness.state_mut().settings_dialog = Some(SettingsDialog);
        harness.run();
        save(&mut harness, "settings");
        harness.key_press(egui::Key::Escape);
        harness.run();

        harness.key_press(egui::Key::F1);
        harness.run();
        save(&mut harness, "shortcuts");
        harness.key_press(egui::Key::Escape);
        harness.run();

        harness.ctx.set_theme(egui::Theme::Light);
        harness.run();
        save(&mut harness, "main-light");
        harness.state_mut().tabs[0].view = crate::tabs::View::Table;
        harness.run();
        save(&mut harness, "table-light");
        harness.state_mut().tabs[0].view = crate::tabs::View::List;
        harness.state_mut().goto = Some(GotoPalette::new());
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Go to collection")
            .type_text("ord");
        harness.run();
        save(&mut harness, "goto-light");
        harness.key_press(egui::Key::Escape);
        harness.run();
        harness.state_mut().tab_action(0, TabAction::Edit(0));
        harness.run();
        save(&mut harness, "edit-document-light");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        harness.ctx.set_theme(egui::Theme::Dark);
        harness.run();

        for (name, width) in [("narrow", 940.0), ("narrowest", 700.0)] {
            harness.set_size(egui::vec2(width, 760.0));
            harness.run();
            save(&mut harness, name);
        }
        harness.set_size(egui::vec2(1440.0, 900.0));
        harness.run();

        harness
            .get_by_role_and_label(Role::Button, "History")
            .click();
        harness.run();
        save(&mut harness, "history");
        harness.key_press(egui::Key::Escape);
        harness.run();

        harness.state_mut().tab_action(0, TabAction::Edit(0));
        harness.run();
        save(&mut harness, "edit-document");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        harness.state_mut().tab_action(0, TabAction::DeleteMany);
        harness.run();
        save(&mut harness, "delete-many");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();

        {
            let tab = &mut harness.state_mut().tabs[0];
            tab.panel = Panel::Indexes;
            tab.indexes = Some(Ok(vec![
                nordpil_backend::IndexInfo {
                    name: "_id_".to_owned(),
                    spec: bson::doc! { "v": 2, "key": { "_id": 1 }, "name": "_id_" },
                    size: Some(36_864),
                    ops: Some(1_204),
                    since: Some(bson::DateTime::from_millis(1_700_000_000_000)),
                },
                nordpil_backend::IndexInfo {
                    name: "status_1_created_-1".to_owned(),
                    spec: bson::doc! {
                        "key": { "status": 1, "created": -1 },
                        "name": "status_1_created_-1",
                        "partialFilterExpression": { "status": { "$exists": true } },
                    },
                    size: Some(2_310_144),
                    ops: Some(88),
                    since: None,
                },
                nordpil_backend::IndexInfo {
                    name: "email_1".to_owned(),
                    spec: bson::doc! { "key": { "customer.email": 1 }, "unique": true, "hidden": true },
                    size: Some(512_000),
                    ops: Some(0),
                    since: None,
                },
            ]));
        }
        harness.run();
        save(&mut harness, "indexes");
        harness.state_mut().index_action(0, IndexAction::Create);
        harness.run();
        save(&mut harness, "create-index");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        harness.state_mut().tabs[0].panel = Panel::Documents;

        {
            use crate::shell::{Body, Selected, ShellResult};
            let tab = &mut harness.state_mut().tabs[0];
            tab.panel = Panel::Shell;
            tab.shell.loaded = true;
            tab.shell.text = "const since = ISODate('2024-01-01')\n\
                db.orders.find({ status: 'shipped', created: { $gte: since } })\n  \
                .sort({ total: -1 })\n\
                db.orders.countDocuments({ status: 'shipped' })\n\
                print('checked', since)"
                .to_owned();
            let documents = tab
                .results
                .as_ref()
                .map(|results| results.documents.clone())
                .unwrap_or_default();
            tab.shell.results = vec![
                ShellResult {
                    title: "db.orders.find({ status: 'shipped', cr…".to_owned(),
                    elapsed: std::time::Duration::from_millis(14),
                    body: Body::documents(documents, 20),
                },
                ShellResult {
                    title: "db.orders.countDocuments({ status: 'sh…".to_owned(),
                    elapsed: std::time::Duration::from_millis(3),
                    body: Body::Value {
                        text: "1234".to_owned(),
                        spans: nordpil_query::highlight("1234"),
                    },
                },
            ];
            tab.shell.output = vec!["checked 2024-01-01T00:00:00.000Z".to_owned()];
            tab.shell.selected = Some(Selected::Result(0));
        }
        harness.run();
        save(&mut harness, "shell");
        harness.state_mut().settings.shell.orientation =
            nordpil_backend::settings::ShellOrientation::SideBySide;
        harness.run();
        save(&mut harness, "shell-side-by-side");
        harness.state_mut().settings.shell = Default::default();
        harness.state_mut().tabs[0].panel = Panel::Documents;

        harness.state_mut().tab_action(0, TabAction::Export);
        harness.run();
        harness.get_by_label("CSV").click();
        harness.run();
        save(&mut harness, "export");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        harness.state_mut().tab_action(0, TabAction::Import);
        harness.run();
        save(&mut harness, "import");
        harness
            .get_by_role_and_label(Role::Button, "Cancel")
            .click();
        harness.run();
        assert!(harness.state().transfer_dialog.is_none());

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
