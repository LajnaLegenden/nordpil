//! Background runtime and database access for nordpil.

mod bridge;
mod connections;
mod databases;
mod documents;
mod error;
mod fields;
mod indexes;
mod queries;
pub mod secrets;
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
pub use documents::{FindQuery, FoundDocument, Namespace, PREVIEW_BYTES};
pub use error::{BackendError, ErrorKind};
pub use fields::FieldInfo;
pub use indexes::IndexInfo;
use mongodb::Client;
use queries::QueryStore;
pub use queries::{QueryText, SavedQuery};
use secrets::SecretStore;
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
    Documents(Result<Vec<FoundDocument>, BackendError>),
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
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<Uuid, Session>>,
}

impl Backend {
    /// Saved queries live in `queries.toml` next to the connections file.
    pub fn new(store: ConnectionStore, secrets: impl SecretStore) -> Self {
        let queries = QueryStore::new(store.path().with_file_name("queries.toml"));
        Self {
            store: Arc::new(tokio::sync::Mutex::new(store)),
            queries: Arc::new(tokio::sync::Mutex::new(queries)),
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

    async fn handle(&self, command: Command, _cancel: CancellationToken) -> Response {
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
