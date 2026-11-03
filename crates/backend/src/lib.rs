//! Background runtime and database access for nordpil.

mod bridge;
mod connections;
mod databases;
mod documents;
mod error;
mod fields;
mod indexes;
pub mod navigation;
mod queries;
pub mod references;
mod scripts;
pub mod secrets;
pub mod settings;
mod shell;
pub mod store;
mod toml_file;
pub mod transfer;
pub mod uri;
mod writes;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

pub use bridge::{Bridge, Event, Handler, Outcome, RequestId};
pub use databases::{CollectionInfo, CollectionKind, DatabaseInfo};
pub use documents::{FindQuery, Namespace, PREVIEW_BYTES};
pub use error::{BackendError, ErrorKind};
pub use fields::FieldInfo;
pub use indexes::IndexInfo;
use mongodb::Client;
use navigation::{KnownDatabase, Navigation, NavigationStore, Recent};
use queries::QueryStore;
pub use queries::{QueryText, SavedQuery};
use references::{DatabaseMappings, ReferenceStore};
use scripts::ScriptStore;
pub use scripts::{SavedScript, Scripts};
use secrets::SecretStore;
use settings::Settings;
pub use shell::{AggregateQuery, SCRIPT_MAX_TIME, ScriptRun};
use store::{ConnectionStore, SavedConnection};
pub use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A password that never shows up in `Debug` output or logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(password: impl Into<String>) -> Self {
        Self(password.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(***)")
    }
}

/// What to do with the stored password when saving a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChange {
    Keep,
    Set(Password),
    Clear,
}

/// Commands the UI sends to the backend.
#[derive(Debug)]
pub enum Command {
    LoadConnections,
    /// A password inside `connection.uri` is moved to the keyring.
    SaveConnection {
        connection: SavedConnection,
        password: PasswordChange,
    },
    DeleteConnection(Uuid),
    /// Without a `password`, the keyring password is used.
    Connect {
        id: Uuid,
        password: Option<Password>,
        remember_password: bool,
    },
    Disconnect(Uuid),
    ListDatabases(Uuid),
    ListCollections {
        connection: Uuid,
        database: String,
    },
    CreateCollection {
        connection: Uuid,
        database: String,
        collection: String,
    },
    DropCollection {
        connection: Uuid,
        database: String,
        collection: String,
    },
    DropDatabase {
        connection: Uuid,
        database: String,
    },
    Find {
        namespace: Namespace,
        query: Box<FindQuery>,
    },
    /// Counts documents matching `filter`, giving up after a few seconds.
    Count {
        namespace: Namespace,
        filter: mongodb::bson::Document,
        hint: Option<mongodb::bson::Bson>,
    },
    /// Samples documents to learn the field paths, for autocomplete.
    SampleFields(Namespace),
    LoadQueries(Namespace),
    /// Adds a query to the collection's history, or moves it to the top.
    RecordQuery {
        namespace: Namespace,
        query: QueryText,
    },
    SetQueryFavorite {
        namespace: Namespace,
        id: Uuid,
        favorite: bool,
    },
    DeleteQuery {
        namespace: Namespace,
        id: Uuid,
    },
    InsertDocuments {
        namespace: Namespace,
        documents: Vec<mongodb::bson::Document>,
    },
    /// Saves only what changed, and fails with [`ErrorKind::Conflict`] if
    /// those fields changed on the server since `original` was loaded.
    UpdateDocument {
        namespace: Namespace,
        original: Box<mongodb::bson::Document>,
        updated: Box<mongodb::bson::Document>,
    },
    DeleteDocument {
        namespace: Namespace,
        id: mongodb::bson::Bson,
    },
    /// `update` is an operator document or a pipeline array.
    UpdateMany {
        namespace: Namespace,
        filter: mongodb::bson::Document,
        update: mongodb::bson::Bson,
    },
    DeleteMany {
        namespace: Namespace,
        filter: mongodb::bson::Document,
    },
    ListIndexes(Namespace),
    CreateIndex {
        namespace: Namespace,
        keys: mongodb::bson::Document,
        /// Spec fields such as `unique` or `expireAfterSeconds`.
        options: mongodb::bson::Document,
    },
    DropIndex {
        namespace: Namespace,
        name: String,
    },
    SetIndexHidden {
        namespace: Namespace,
        name: String,
        hidden: bool,
    },
    Export(Box<transfer::ExportJob>),
    Import(Box<transfer::ImportJob>),
    SaveSettings(Settings),
    /// Recent collections and remembered collection names, for Ctrl+K.
    LoadNavigation,
    /// Replaces the recent collections, newest first.
    SaveRecents(Vec<Recent>),
    /// Lists the collections of every database. With `remember`, they are
    /// kept on disk so Ctrl+K finds them before connecting.
    IndexCollections {
        connection: Uuid,
        remember: bool,
    },
    /// Forgets one connection's remembered collection names, or all of them.
    ForgetCollections(Option<Uuid>),
    /// The fields users linked to collections, for every database.
    LoadMappings,
    /// Replaces every database's linked fields.
    SaveMappings(Vec<DatabaseMappings>),
    /// Runs a shell script with `db` set to the namespace's database.
    /// Results arrive through `run` while it goes.
    RunScript {
        namespace: Namespace,
        script: String,
        run: Arc<ScriptRun>,
    },
    /// A page of an aggregation from a shell result.
    Aggregate {
        namespace: Namespace,
        query: Box<AggregateQuery>,
    },
    LoadScripts(Namespace),
    /// Adds a script to the collection's history, or moves it to the top.
    RecordScript {
        namespace: Namespace,
        script: String,
    },
    SetScriptFavorite {
        namespace: Namespace,
        id: Uuid,
        favorite: bool,
    },
    DeleteScript {
        namespace: Namespace,
        id: Uuid,
    },
    /// Keeps the shell editor's text; empty forgets it.
    SaveDraft {
        namespace: Namespace,
        text: String,
    },
}

/// Results of [`Command`]s, with enough context to act on them.
#[derive(Debug)]
pub enum Response {
    Connections {
        result: Result<Vec<SavedConnection>, BackendError>,
        keyring_available: bool,
    },
    ConnectionSaved(Result<SavedConnection, BackendError>),
    ConnectionDeleted {
        id: Uuid,
        result: Result<(), BackendError>,
    },
    Connected {
        id: Uuid,
        result: Result<Vec<DatabaseInfo>, BackendError>,
    },
    Disconnected(Uuid),
    Databases {
        connection: Uuid,
        result: Result<Vec<DatabaseInfo>, BackendError>,
    },
    Collections {
        connection: Uuid,
        database: String,
        result: Result<Vec<CollectionInfo>, BackendError>,
    },
    CollectionCreated {
        connection: Uuid,
        database: String,
        result: Result<(), BackendError>,
    },
    CollectionDropped {
        connection: Uuid,
        database: String,
        result: Result<(), BackendError>,
    },
    DatabaseDropped {
        connection: Uuid,
        database: String,
        result: Result<(), BackendError>,
    },
    /// Matched to its query through the request id.
    Documents(Result<Vec<mongodb::bson::Document>, BackendError>),
    /// `None` when counting took too long.
    Count(Result<Option<u64>, BackendError>),
    Fields {
        namespace: Namespace,
        result: Result<Vec<FieldInfo>, BackendError>,
    },
    /// Any document write; the count is how many documents it affected.
    Written {
        namespace: Namespace,
        result: Result<u64, BackendError>,
    },
    Indexes {
        namespace: Namespace,
        result: Result<Vec<IndexInfo>, BackendError>,
    },
    /// After creating, dropping or hiding an index.
    IndexChanged {
        namespace: Namespace,
        result: Result<(), BackendError>,
    },
    /// How many documents were exported.
    Exported(Result<u64, BackendError>),
    Imported {
        namespace: Namespace,
        result: Result<transfer::ImportSummary, BackendError>,
    },
    /// The collection's saved queries after a query command: favorites first,
    /// then most recent.
    Queries {
        namespace: Namespace,
        result: Result<Vec<SavedQuery>, BackendError>,
    },
    Navigation(Result<Navigation, BackendError>),
    Mappings(Result<Vec<DatabaseMappings>, BackendError>),
    CollectionIndex {
        connection: Uuid,
        result: Result<Vec<KnownDatabase>, BackendError>,
    },
    /// After a command that only writes a file, such as saving settings.
    Stored(Result<(), BackendError>),
    ScriptFinished(Result<nordpil_shell::Outcome, BackendError>),
    /// The collection's scripts after a script command.
    Scripts {
        namespace: Namespace,
        result: Result<Scripts, BackendError>,
    },
}

/// An open connection.
#[derive(Clone)]
struct Session {
    client: Client,
    read_only: bool,
}

/// Handles [`Command`]s on the background runtime.
pub struct Backend {
    store: Arc<tokio::sync::Mutex<ConnectionStore>>,
    queries: Arc<tokio::sync::Mutex<QueryStore>>,
    navigation: Arc<tokio::sync::Mutex<NavigationStore>>,
    references: Arc<tokio::sync::Mutex<ReferenceStore>>,
    scripts: Arc<tokio::sync::Mutex<ScriptStore>>,
    settings_path: std::path::PathBuf,
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<Uuid, Session>>,
}

impl Backend {
    /// Saved queries, settings, navigation and linked fields live next to the
    /// connections file.
    pub fn new(store: ConnectionStore, secrets: impl SecretStore) -> Self {
        let queries = QueryStore::new(store.path().with_file_name("queries.toml"));
        let dir = store.path().parent().unwrap_or(std::path::Path::new("."));
        let navigation = NavigationStore::new(dir);
        let references = ReferenceStore::new(dir.join("references.toml"));
        let scripts = ScriptStore::new(dir.join("scripts.toml"));
        let settings_path = store.path().with_file_name("config.toml");
        Self {
            store: Arc::new(tokio::sync::Mutex::new(store)),
            queries: Arc::new(tokio::sync::Mutex::new(queries)),
            navigation: Arc::new(tokio::sync::Mutex::new(navigation)),
            references: Arc::new(tokio::sync::Mutex::new(references)),
            scripts: Arc::new(tokio::sync::Mutex::new(scripts)),
            settings_path,
            secrets: Arc::new(secrets),
            sessions: Mutex::default(),
        }
    }

    fn session(&self, id: Uuid) -> Result<Session, BackendError> {
        self.lock_sessions()
            .get(&id)
            .cloned()
            .ok_or_else(|| BackendError::new("Not connected."))
    }

    fn writable_session(&self, id: Uuid) -> Result<Session, BackendError> {
        let session = self.session(id)?;
        if session.read_only {
            return Err(BackendError::read_only());
        }
        Ok(session)
    }

    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Session>> {
        // No code panics while holding the lock, so a poisoned map is still consistent.
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs `f` on a blocking thread with exclusive access to the store.
    async fn with_store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ConnectionStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let store = Arc::clone(&self.store).lock_owned().await;
        blocking(move || f(&store)).await
    }

    async fn with_queries<T: Send + 'static>(
        &self,
        f: impl FnOnce(&QueryStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let queries = Arc::clone(&self.queries).lock_owned().await;
        blocking(move || f(&queries)).await
    }

    async fn with_scripts<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ScriptStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let scripts = Arc::clone(&self.scripts).lock_owned().await;
        blocking(move || f(&scripts)).await
    }

    async fn with_secrets<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn SecretStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let secrets = Arc::clone(&self.secrets);
        blocking(move || f(secrets.as_ref())).await
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, BackendError> + Send + 'static,
) -> Result<T, BackendError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| BackendError::new("A background task failed.").with_detail(error))?
}

impl Handler for Backend {
    type Command = Command;
    type Response = Response;

    async fn handle(&self, command: Command, cancel: CancellationToken) -> Response {
        match command {
            Command::LoadConnections => Response::Connections {
                result: self.with_store(ConnectionStore::load).await,
                keyring_available: self.with_secrets(|s| Ok(s.available())).await == Ok(true),
            },
            Command::SaveConnection {
                connection,
                password,
            } => Response::ConnectionSaved(self.save_connection(connection, password).await),
            Command::DeleteConnection(id) => Response::ConnectionDeleted {
                id,
                result: self.delete_connection(id).await,
            },
            Command::Connect {
                id,
                password,
                remember_password,
            } => Response::Connected {
                id,
                result: self.connect(id, password, remember_password).await,
            },
            Command::Disconnect(id) => {
                self.lock_sessions().remove(&id);
                Response::Disconnected(id)
            }
            Command::ListDatabases(connection) => Response::Databases {
                connection,
                result: self.list_databases(connection).await,
            },
            Command::ListCollections {
                connection,
                database,
            } => Response::Collections {
                result: self.list_collections(connection, &database).await,
                connection,
                database,
            },
            Command::CreateCollection {
                connection,
                database,
                collection,
            } => Response::CollectionCreated {
                result: self
                    .create_collection(connection, &database, &collection)
                    .await,
                connection,
                database,
            },
            Command::DropCollection {
                connection,
                database,
                collection,
            } => Response::CollectionDropped {
                result: self
                    .drop_collection(connection, &database, &collection)
                    .await,
                connection,
                database,
            },
            Command::DropDatabase {
                connection,
                database,
            } => Response::DatabaseDropped {
                result: self.drop_database(connection, &database).await,
                connection,
                database,
            },
            Command::Find { namespace, query } => {
                Response::Documents(self.find(&namespace, *query).await)
            }
            Command::Count {
                namespace,
                filter,
                hint,
            } => Response::Count(self.count(&namespace, filter, hint).await),
            Command::SampleFields(namespace) => Response::Fields {
                result: self.sample_fields(&namespace).await,
                namespace,
            },
            Command::LoadQueries(namespace) => Response::Queries {
                result: self.load_queries(namespace.clone()).await,
                namespace,
            },
            Command::RecordQuery { namespace, query } => Response::Queries {
                result: self.record_query(namespace.clone(), query).await,
                namespace,
            },
            Command::SetQueryFavorite {
                namespace,
                id,
                favorite,
            } => Response::Queries {
                result: self.set_favorite(namespace.clone(), id, favorite).await,
                namespace,
            },
            Command::DeleteQuery { namespace, id } => Response::Queries {
                result: self.delete_query(namespace.clone(), id).await,
                namespace,
            },
            Command::InsertDocuments {
                namespace,
                documents,
            } => Response::Written {
                result: self.insert_documents(&namespace, documents).await,
                namespace,
            },
            Command::UpdateDocument {
                namespace,
                original,
                updated,
            } => Response::Written {
                result: self
                    .update_document(&namespace, &original, &updated)
                    .await
                    .map(|()| 1),
                namespace,
            },
            Command::DeleteDocument { namespace, id } => Response::Written {
                result: self.delete_document(&namespace, id).await.map(|()| 1),
                namespace,
            },
            Command::UpdateMany {
                namespace,
                filter,
                update,
            } => Response::Written {
                result: self.update_many(&namespace, filter, update).await,
                namespace,
            },
            Command::DeleteMany { namespace, filter } => Response::Written {
                result: self.delete_many(&namespace, filter).await,
                namespace,
            },
            Command::ListIndexes(namespace) => Response::Indexes {
                result: self.list_indexes(&namespace).await,
                namespace,
            },
            Command::CreateIndex {
                namespace,
                keys,
                options,
            } => Response::IndexChanged {
                result: self.create_index(&namespace, keys, options).await,
                namespace,
            },
            Command::DropIndex { namespace, name } => Response::IndexChanged {
                result: self.drop_index(&namespace, &name).await,
                namespace,
            },
            Command::SetIndexHidden {
                namespace,
                name,
                hidden,
            } => Response::IndexChanged {
                result: self.set_index_hidden(&namespace, &name, hidden).await,
                namespace,
            },
            Command::Export(job) => Response::Exported(self.export(*job).await),
            Command::Import(job) => {
                let namespace = job.namespace.clone();
                Response::Imported {
                    result: self.import(*job).await,
                    namespace,
                }
            }
            Command::SaveSettings(settings) => Response::Stored(self.save_settings(settings).await),
            Command::LoadNavigation => Response::Navigation(self.load_navigation().await),
            Command::SaveRecents(recents) => Response::Stored(self.save_recents(recents).await),
            Command::IndexCollections {
                connection,
                remember,
            } => Response::CollectionIndex {
                connection,
                result: self.index_collections(connection, remember).await,
            },
            Command::ForgetCollections(connection) => {
                Response::Stored(self.forget_collections(connection).await)
            }
            Command::LoadMappings => Response::Mappings(self.load_mappings().await),
            Command::SaveMappings(databases) => {
                Response::Stored(self.save_mappings(databases).await)
            }
            Command::RunScript {
                namespace,
                script,
                run,
            } => Response::ScriptFinished(self.run_script(&namespace, script, run, cancel).await),
            Command::Aggregate { namespace, query } => {
                Response::Documents(self.aggregate(&namespace, *query).await)
            }
            Command::LoadScripts(namespace) => Response::Scripts {
                result: self.load_scripts(namespace.clone()).await,
                namespace,
            },
            Command::RecordScript { namespace, script } => Response::Scripts {
                result: self.record_script(namespace.clone(), script).await,
                namespace,
            },
            Command::SetScriptFavorite {
                namespace,
                id,
                favorite,
            } => Response::Scripts {
                result: self
                    .set_script_favorite(namespace.clone(), id, favorite)
                    .await,
                namespace,
            },
            Command::DeleteScript { namespace, id } => Response::Scripts {
                result: self.delete_script(namespace.clone(), id).await,
                namespace,
            },
            Command::SaveDraft { namespace, text } => {
                Response::Stored(self.save_draft(namespace, text).await)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;

    fn backend() -> Backend {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        Backend::new(
            ConnectionStore::new(dir.join("connections.toml")),
            MemorySecrets::default(),
        )
    }

    fn connection(uri: &str) -> SavedConnection {
        SavedConnection {
            id: Uuid::new_v4(),
            name: String::new(),
            uri: uri.to_owned(),
            color: None,
            read_only: false,
        }
    }

    async fn send(backend: &Backend, command: Command) -> Response {
        backend.handle(command, CancellationToken::new()).await
    }

    #[tokio::test]
    async fn save_moves_password_from_uri_to_secret_store() {
        let backend = backend();
        let connection = connection("mongodb://bob:s3cret@localhost/");

        let Response::ConnectionSaved(saved) = send(
            &backend,
            Command::SaveConnection {
                connection: connection.clone(),
                password: PasswordChange::Keep,
            },
        )
        .await
        else {
            panic!("unexpected response");
        };

        let saved = saved.unwrap();
        assert_eq!(saved.uri, "mongodb://bob@localhost");
        assert_eq!(saved.name, "localhost");
        assert_eq!(
            backend.secrets.get(connection.id).unwrap().as_deref(),
            Some("s3cret")
        );
        let Response::Connections { result, .. } = send(&backend, Command::LoadConnections).await
        else {
            panic!("unexpected response");
        };
        assert_eq!(result.unwrap(), [saved]);
    }

    #[tokio::test]
    async fn connect_asks_for_missing_password() {
        let backend = backend();
        let connection = connection("mongodb://bob@localhost:1/");
        let id = connection.id;
        send(
            &backend,
            Command::SaveConnection {
                connection,
                password: PasswordChange::Keep,
            },
        )
        .await;

        let Response::Connected { result, .. } = send(
            &backend,
            Command::Connect {
                id,
                password: None,
                remember_password: false,
            },
        )
        .await
        else {
            panic!("unexpected response");
        };

        assert_eq!(result.unwrap_err().kind, ErrorKind::PasswordRequired);
    }
}
