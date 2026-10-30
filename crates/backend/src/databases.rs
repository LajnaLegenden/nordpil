//! Listing, creating and dropping databases and collections.

use futures_util::TryStreamExt as _;
use mongodb::Client;
use mongodb::bson::Document;
use mongodb::results::CollectionType;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Backend, BackendError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub name: String,
    pub size_on_disk: u64,
    pub empty: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    #[default]
    Collection,
    View,
    Timeseries,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionInfo {
    pub name: String,
    #[serde(default)]
    pub kind: CollectionKind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capped: bool,
}

/// Databases the user may access, sorted by name.
pub(crate) async fn list(client: &Client) -> Result<Vec<DatabaseInfo>, BackendError> {
    let mut databases: Vec<_> = client
        .list_databases()
        .authorized_databases(true)
        .await?
        .into_iter()
        .map(|spec| DatabaseInfo {
            name: spec.name,
            size_on_disk: spec.size_on_disk,
            empty: spec.empty,
        })
        .collect();
    databases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(databases)
}

impl Backend {
    pub(crate) async fn list_databases(
        &self,
        connection: Uuid,
    ) -> Result<Vec<DatabaseInfo>, BackendError> {
        list(&self.session(connection)?.client).await
    }

    pub(crate) async fn list_collections(
        &self,
        connection: Uuid,
        database: &str,
    ) -> Result<Vec<CollectionInfo>, BackendError> {
        let client = self.session(connection)?.client;
        let specs: Vec<_> = client
            .database(database)
            .list_collections()
            .await?
            .try_collect()
            .await?;
        let mut collections: Vec<_> = specs
            .into_iter()
            .map(|spec| CollectionInfo {
                kind: match spec.collection_type {
                    CollectionType::View => CollectionKind::View,
                    CollectionType::Timeseries => CollectionKind::Timeseries,
                    _ => CollectionKind::Collection,
                },
                capped: spec.options.capped.unwrap_or(false),
                name: spec.name,
            })
            .collect();
        collections.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(collections)
    }

    /// Also creates the database when it doesn't exist yet.
    pub(crate) async fn create_collection(
        &self,
        connection: Uuid,
        database: &str,
        collection: &str,
    ) -> Result<(), BackendError> {
        require_name("database", database)?;
        require_name("collection", collection)?;
        let client = self.writable_session(connection)?.client;
        client
            .database(database)
            .create_collection(collection)
            .await?;
        Ok(())
    }

    pub(crate) async fn drop_collection(
        &self,
        connection: Uuid,
        database: &str,
        collection: &str,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(connection)?.client;
        client
            .database(database)
            .collection::<Document>(collection)
            .drop()
            .await?;
        Ok(())
    }

    pub(crate) async fn drop_database(
        &self,
        connection: Uuid,
        database: &str,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(connection)?.client;
        client.database(database).drop().await?;
        Ok(())
    }
}

fn require_name(what: &str, name: &str) -> Result<(), BackendError> {
    if name.trim().is_empty() {
        return Err(BackendError::new(format!("Enter a {what} name.")));
    }
    Ok(())
}
