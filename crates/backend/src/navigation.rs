//! What Ctrl+K needs: recently opened collections, and the collection names
//! of saved connections so they can be found before connecting. Kept in
//! `recent.toml` and `collections.toml` next to the saved connections.

use std::path::PathBuf;

use futures_util::{StreamExt as _, stream};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::databases::CollectionInfo;
use crate::{Backend, BackendError, toml_file};

const FILE_VERSION: u32 = 1;
/// Databases listed per connection; past this, the rest are skipped.
const MAX_DATABASES: usize = 200;
/// Databases listed at the same time.
const PARALLEL_LISTS: usize = 8;

/// A collection the user opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
    /// Milliseconds since 1970.
    pub opened_at: i64,
}

/// A database's collections as last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownDatabase {
    pub connection: Uuid,
    pub database: String,
    #[serde(default, rename = "collection")]
    pub collections: Vec<CollectionInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Navigation {
    /// Newest first.
    pub recents: Vec<Recent>,
    pub databases: Vec<KnownDatabase>,
}

#[derive(Serialize, Deserialize)]
struct RecentFile {
    version: u32,
    #[serde(default, rename = "recent")]
    recents: Vec<Recent>,
}

#[derive(Serialize, Deserialize)]
struct CollectionsFile {
    version: u32,
    #[serde(default, rename = "database")]
    databases: Vec<KnownDatabase>,
}

pub(crate) struct NavigationStore {
    recent_path: PathBuf,
    collections_path: PathBuf,
}

impl NavigationStore {
    pub(crate) fn new(dir: &std::path::Path) -> Self {
        Self {
            recent_path: dir.join("recent.toml"),
            collections_path: dir.join("collections.toml"),
        }
    }

    fn load(&self) -> Result<Navigation, BackendError> {
        let recents = match toml_file::read::<RecentFile>(&self.recent_path)? {
            Some(file) if file.version > FILE_VERSION => {
                return Err(toml_file::newer_version_error(&self.recent_path));
            }
            Some(file) => file.recents,
            None => Vec::new(),
        };
        Ok(Navigation {
            recents,
            databases: self.load_databases()?,
        })
    }

    fn load_databases(&self) -> Result<Vec<KnownDatabase>, BackendError> {
        match toml_file::read::<CollectionsFile>(&self.collections_path)? {
            Some(file) if file.version > FILE_VERSION => {
                Err(toml_file::newer_version_error(&self.collections_path))
            }
            Some(file) => Ok(file.databases),
            None => Ok(Vec::new()),
        }
    }

    fn save_recents(&self, recents: Vec<Recent>) -> Result<(), BackendError> {
        let file = RecentFile {
            version: FILE_VERSION,
            recents,
        };
        toml_file::write(&self.recent_path, &file)
    }

    fn save_databases(&self, databases: Vec<KnownDatabase>) -> Result<(), BackendError> {
        if databases.is_empty() {
            return match std::fs::remove_file(&self.collections_path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(BackendError::new(format!(
                        "Could not remove {}.",
                        self.collections_path.display()
                    ))
                    .with_detail(error))
                }
                _ => Ok(()),
            };
        }
        let file = CollectionsFile {
            version: FILE_VERSION,
            databases,
        };
        toml_file::write(&self.collections_path, &file)
    }

    /// Replaces what is known about one connection.
    fn replace(&self, connection: Uuid, mut found: Vec<KnownDatabase>) -> Result<(), BackendError> {
        let mut databases = self.load_databases()?;
        databases.retain(|known| known.connection != connection);
        databases.append(&mut found);
        self.save_databases(databases)
    }

    /// Forgets one connection's collections, or every connection's.
    fn forget(&self, connection: Option<Uuid>) -> Result<(), BackendError> {
        let mut databases = match connection {
            Some(_) => self.load_databases()?,
            None => Vec::new(),
        };
        databases.retain(|known| Some(known.connection) != connection);
        self.save_databases(databases)
    }
}

impl Backend {
    async fn with_navigation<T: Send + 'static>(
        &self,
        f: impl FnOnce(&NavigationStore) -> Result<T, BackendError> + Send + 'static,
    ) -> Result<T, BackendError> {
        let store = std::sync::Arc::clone(&self.navigation).lock_owned().await;
        crate::blocking(move || f(&store)).await
    }

    pub(crate) async fn load_navigation(&self) -> Result<Navigation, BackendError> {
        self.with_navigation(NavigationStore::load).await
    }

    pub(crate) async fn save_recents(&self, recents: Vec<Recent>) -> Result<(), BackendError> {
        self.with_navigation(move |store| store.save_recents(recents))
            .await
    }

    pub(crate) async fn forget_collections(
        &self,
        connection: Option<Uuid>,
    ) -> Result<(), BackendError> {
        self.with_navigation(move |store| store.forget(connection))
            .await
    }

    /// Lists every database's collections. Databases the user can't list are
    /// left out. With `remember`, the result replaces what `collections.toml`
    /// knows about the connection.
    pub(crate) async fn index_collections(
        &self,
        connection: Uuid,
        remember: bool,
    ) -> Result<Vec<KnownDatabase>, BackendError> {
        let databases = self.list_databases(connection).await?;
        let found: Vec<KnownDatabase> = stream::iter(databases.into_iter().take(MAX_DATABASES))
            .map(|database| async move {
                let collections = self.list_collections(connection, &database.name).await;
                collections.ok().map(|collections| KnownDatabase {
                    connection,
                    database: database.name,
                    collections,
                })
            })
            .buffered(PARALLEL_LISTS)
            .filter_map(std::future::ready)
            .collect()
            .await;
        if remember {
            let saved = found.clone();
            self.with_navigation(move |store| store.replace(connection, saved))
                .await?;
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CollectionKind;

    fn store() -> (NavigationStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        (NavigationStore::new(&dir), dir)
    }

    fn known(connection: Uuid, database: &str, collections: &[&str]) -> KnownDatabase {
        KnownDatabase {
            connection,
            database: database.to_owned(),
            collections: collections
                .iter()
                .map(|name| CollectionInfo {
                    name: (*name).to_owned(),
                    kind: CollectionKind::Collection,
                    capped: false,
                })
                .collect(),
        }
    }

    #[test]
    fn replaces_and_forgets_one_connection() {
        let (store, dir) = store();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(store.load().unwrap(), Navigation::default());

        store
            .replace(a, vec![known(a, "shop", &["orders", "users"])])
            .unwrap();
        store
            .replace(b, vec![known(b, "logs", &["events"])])
            .unwrap();
        store
            .replace(a, vec![known(a, "shop", &["orders"])])
            .unwrap();
        assert_eq!(
            store.load().unwrap().databases,
            [known(b, "logs", &["events"]), known(a, "shop", &["orders"])]
        );

        store.forget(Some(b)).unwrap();
        assert_eq!(
            store.load().unwrap().databases,
            [known(a, "shop", &["orders"])]
        );
        store.forget(None).unwrap();
        assert!(!store.collections_path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_and_loads_recents() {
        let (store, dir) = store();
        let recents = vec![Recent {
            connection: Uuid::new_v4(),
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
            opened_at: 1_700_000_000_000,
        }];

        store.save_recents(recents.clone()).unwrap();

        assert_eq!(store.load().unwrap().recents, recents);
        let _ = std::fs::remove_dir_all(dir);
    }
}
