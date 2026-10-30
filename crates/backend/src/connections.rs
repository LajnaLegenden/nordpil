//! Saving, deleting and opening connections.

use std::time::Duration;

use mongodb::Client;
use mongodb::bson::doc;
use mongodb::options::ClientOptions;
use uuid::Uuid;

use crate::store::SavedConnection;
use crate::uri::ConnectionUri;
use crate::{Backend, BackendError, DatabaseInfo, ErrorKind, Password, PasswordChange, Session};

/// Used unless the connection string sets its own timeouts. The driver's
/// 30 s default is too long to stare at a spinner.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

impl Backend {
    pub(crate) async fn save_connection(
        &self,
        mut connection: SavedConnection,
        password: PasswordChange,
    ) -> Result<SavedConnection, BackendError> {
        let uri = parse_uri(&connection.uri)?;
        let password = match (password, &uri.password) {
            (PasswordChange::Keep, Some(in_uri)) => PasswordChange::Set(Password::new(in_uri)),
            (change, _) => change,
        };
        connection.uri = uri.without_password().to_string();
        if connection.name.trim().is_empty() {
            connection.name = uri.hosts.join(",");
        }

        let id = connection.id;
        self.with_secrets(move |secrets| match password {
            _ if !secrets.available() => Ok(()),
            PasswordChange::Keep => Ok(()),
            PasswordChange::Set(password) => secrets.set(id, password.expose()),
            PasswordChange::Clear => secrets.delete(id),
        })
        .await?;

        let saved = connection.clone();
        self.with_store(move |store| {
            let mut connections = store.load()?;
            match connections.iter_mut().find(|c| c.id == saved.id) {
                Some(existing) => *existing = saved,
                None => connections.push(saved),
            }
            store.save(&connections)
        })
        .await?;

        // Turning read-only on must apply to an open session right away.
        if let Some(session) = self.lock_sessions().get_mut(&id) {
            session.read_only = connection.read_only;
        }
        Ok(connection)
    }

    pub(crate) async fn delete_connection(&self, id: Uuid) -> Result<(), BackendError> {
        self.lock_sessions().remove(&id);
        self.with_store(move |store| {
            let mut connections = store.load()?;
            connections.retain(|c| c.id != id);
            store.save(&connections)
        })
        .await?;

        let deleted = self
            .with_secrets(move |secrets| {
                if secrets.available() {
                    secrets.delete(id)
                } else {
                    Ok(())
                }
            })
            .await;
        if let Err(error) = deleted {
            tracing::warn!(%id, %error, detail = ?error.detail, "could not delete stored password");
        }
        Ok(())
    }

    pub(crate) async fn connect(
        &self,
        id: Uuid,
        password: Option<Password>,
        remember_password: bool,
    ) -> Result<Vec<DatabaseInfo>, BackendError> {
        let saved = self
            .with_store(move |store| {
                store
                    .load()?
                    .into_iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| BackendError::new("This connection no longer exists."))
            })
            .await?;
        let uri = parse_uri(&saved.uri)?;

        let typed = password.is_some();
        let password = match password {
            Some(password) => Some(password),
            None if uri.username.is_some() => self.stored_password(id).await,
            None => None,
        };
        if uri.username.is_some() && password.is_none() {
            return Err(BackendError::new("Enter the password for this connection.")
                .with_kind(ErrorKind::PasswordRequired));
        }

        let mut options = ClientOptions::parse(saved.uri.as_str()).await?;
        if let (Some(credential), Some(password)) = (options.credential.as_mut(), &password) {
            credential.password = Some(password.expose().to_owned());
        }
        options.app_name.get_or_insert_with(|| "nordpil".to_owned());
        options
            .server_selection_timeout
            .get_or_insert(DEFAULT_TIMEOUT);
        options.connect_timeout.get_or_insert(DEFAULT_TIMEOUT);
        let client = Client::with_options(options)?;
        // Forces server selection and authentication, so bad settings fail here.
        client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await?;
        let databases = crate::databases::list(&client).await?;

        if typed
            && remember_password
            && let Some(password) = password
        {
            let stored = self
                .with_secrets(move |secrets| {
                    if secrets.available() {
                        secrets.set(id, password.expose())
                    } else {
                        Ok(())
                    }
                })
                .await;
            if let Err(error) = stored {
                tracing::warn!(%id, %error, detail = ?error.detail, "could not store password");
            }
        }

        let session = Session {
            client,
            read_only: saved.read_only,
        };
        self.lock_sessions().insert(id, session);
        tracing::info!(%id, "connected");
        Ok(databases)
    }

    async fn stored_password(&self, id: Uuid) -> Option<Password> {
        let stored = self
            .with_secrets(move |secrets| {
                if secrets.available() {
                    secrets.get(id)
                } else {
                    Ok(None)
                }
            })
            .await;
        stored
            .unwrap_or_else(|error| {
                tracing::warn!(%id, %error, detail = ?error.detail, "could not read stored password");
                None
            })
            .map(Password::new)
    }
}

fn parse_uri(uri: &str) -> Result<ConnectionUri, BackendError> {
    ConnectionUri::parse(uri).map_err(|error| BackendError::new(error.to_string()))
}
