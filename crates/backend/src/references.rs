//! Fields the user linked to a collection, per saved connection and
//! database: "`ownerRef` holds a `users` id". Kept in `references.toml` next
//! to the saved connections.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Backend, BackendError, toml_file};

const FILE_VERSION: u32 = 1;

/// A field name, at any depth, whose values are found in `collection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    pub key: String,
    pub collection: String,
    /// The field in `collection` the values match; `_id` when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// One database's mappings; a key appears at most once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseMappings {
    pub connection: Uuid,
    pub database: String,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
}

#[derive(Serialize, Deserialize)]
struct ReferencesFile {
    version: u32,
    #[serde(default, rename = "database")]
    databases: Vec<DatabaseMappings>,
}

pub(crate) struct ReferenceStore {
    path: PathBuf,
}

impl ReferenceStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<Vec<DatabaseMappings>, BackendError> {
        match toml_file::read::<ReferencesFile>(&self.path)? {
            Some(file) if file.version > FILE_VERSION => {
                Err(toml_file::newer_version_error(&self.path))
            }
            Some(file) => Ok(file.databases),
            None => Ok(Vec::new()),
        }
    }

    fn save(&self, mut databases: Vec<DatabaseMappings>) -> Result<(), BackendError> {
        databases.retain(|database| !database.mappings.is_empty());
        let file = ReferencesFile {
            version: FILE_VERSION,
            databases,
        };
        toml_file::write(&self.path, &file)
    }
}

impl Backend {
    async fn with_references<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ReferenceStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let store = std::sync::Arc::clone(&self.references).lock_owned().await;
        crate::blocking(move || f(&store)).await
    }

    pub(crate) async fn load_mappings(&self) -> Result<Vec<DatabaseMappings>, BackendError> {
        self.with_references(ReferenceStore::load).await
    }

    /// Replaces every database's mappings; databases without any are left out.
    pub(crate) async fn save_mappings(
        &self,
        databases: Vec<DatabaseMappings>,
    ) -> Result<(), BackendError> {
        self.with_references(move |store| store.save(databases))
            .await
    }

    pub(crate) async fn forget_mappings(&self, connection: Uuid) -> Result<(), BackendError> {
        self.with_references(move |store| {
            let mut databases = store.load()?;
            databases.retain(|database| database.connection != connection);
            store.save(databases)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use crate::store::{ConnectionStore, SavedConnection};

    fn mappings(connection: Uuid, database: &str, keys: &[(&str, &str)]) -> DatabaseMappings {
        DatabaseMappings {
            connection,
            database: database.to_owned(),
            mappings: keys
                .iter()
                .map(|&(key, collection)| Mapping {
                    key: key.to_owned(),
                    collection: collection.to_owned(),
                    field: (key == "sku").then(|| "code".to_owned()),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn saves_loads_and_forgets_a_deleted_connection() {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        let store = ConnectionStore::new(dir.join("connections.toml"));
        let saved = |name: &str| SavedConnection {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            uri: "mongodb://localhost".to_owned(),
            color: None,
            read_only: false,
        };
        let (a, b) = (saved("a"), saved("b"));
        store.save(&[a.clone(), b.clone()]).unwrap();
        let backend = Backend::new(store, MemorySecrets::default());
        assert!(backend.load_mappings().await.unwrap().is_empty());

        let shop = mappings(a.id, "shop", &[("ownerRef", "users"), ("sku", "products")]);
        let logs = mappings(b.id, "logs", &[("userId", "accounts")]);
        let empty = mappings(b.id, "empty", &[]);
        backend
            .save_mappings(vec![shop.clone(), logs.clone(), empty])
            .await
            .unwrap();

        assert_eq!(backend.load_mappings().await.unwrap(), [shop.clone(), logs]);
        let text = std::fs::read_to_string(dir.join("references.toml")).unwrap();
        assert!(text.contains("field = \"code\""), "{text}");

        backend.delete_connection(b.id).await.unwrap();
        assert_eq!(backend.load_mappings().await.unwrap(), [shop]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
