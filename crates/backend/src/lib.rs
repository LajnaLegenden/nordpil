//! Background runtime and database access for nordpil.

mod bridge;
mod connections;
mod databases;
mod error;
pub mod secrets;
pub mod store;
pub mod uri;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

pub use bridge::{Bridge, Event, Handler, Outcome, RequestId};
pub use databases::{CollectionInfo, CollectionKind, DatabaseInfo};
pub use error::{BackendError, ErrorKind};
use mongodb::Client;
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
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<Uuid, Session>>,
}

impl Backend {
    pub fn new(store: ConnectionStore, secrets: impl SecretStore) -> Self {
        Self {
            store: Arc::new(tokio::sync::Mutex::new(store)),
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
                result: self.drop_collection(connection, &database, &collection).await,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<Uuid, String>>);

    impl SecretStore for MemorySecrets {
        fn available(&self) -> bool {
            true
        }

        fn get(&self, id: Uuid) -> Result<Option<String>, BackendError> {
            Ok(self.0.lock().unwrap().get(&id).cloned())
        }

        fn set(&self, id: Uuid, password: &str) -> Result<(), BackendError> {
            self.0.lock().unwrap().insert(id, password.to_owned());
            Ok(())
        }

        fn delete(&self, id: Uuid) -> Result<(), BackendError> {
            self.0.lock().unwrap().remove(&id);
            Ok(())
        }
    }

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
