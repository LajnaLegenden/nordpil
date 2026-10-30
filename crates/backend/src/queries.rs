//! Recent and favorite queries per collection, kept in `queries.toml` next
//! to the saved connections.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Backend, BackendError, Namespace, toml_file};

/// Older non-favorite queries beyond this many per collection are dropped.
const RECENT_PER_COLLECTION: usize = 20;
const FILE_VERSION: u32 = 1;

/// Query inputs as typed, so they come back exactly as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryText {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sort: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub collation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skip: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub limit: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub max_time_ms: String,
}

impl QueryText {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: Uuid,
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
    #[serde(default)]
    pub favorite: bool,
    /// Milliseconds since 1970.
    pub used_at: i64,
    pub query: QueryText,
}

impl SavedQuery {
    fn is_in(&self, namespace: &Namespace) -> bool {
        self.connection == namespace.connection
            && self.database == namespace.database
            && self.collection == namespace.collection
    }
}

#[derive(Serialize, Deserialize)]
struct QueriesFile {
    version: u32,
    #[serde(default, rename = "query")]
    queries: Vec<SavedQuery>,
}

pub struct QueryStore {
    path: PathBuf,
}

impl QueryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<Vec<SavedQuery>, BackendError> {
        let Some(file) = toml_file::read::<QueriesFile>(&self.path)? else {
            return Ok(Vec::new());
        };
        if file.version > FILE_VERSION {
            return Err(toml_file::newer_version_error(&self.path));
        }
        Ok(file.queries)
    }

    fn save(&self, queries: Vec<SavedQuery>) -> Result<(), BackendError> {
        let file = QueriesFile {
            version: FILE_VERSION,
            queries,
        };
        toml_file::write(&self.path, &file)
    }

    /// Loads, changes and saves the file, then returns the collection's list.
    fn change(
        &self,
        namespace: &Namespace,
        change: impl FnOnce(&mut Vec<SavedQuery>),
    ) -> Result<Vec<SavedQuery>, BackendError> {
        let mut queries = self.load()?;
        change(&mut queries);
        let list = list(&queries, namespace);
        self.save(queries)?;
        Ok(list)
    }
}

/// Favorites first, then the most recently used.
fn list(queries: &[SavedQuery], namespace: &Namespace) -> Vec<SavedQuery> {
    let mut list: Vec<_> = queries
        .iter()
        .filter(|q| q.is_in(namespace))
        .cloned()
        .collect();
    list.sort_by(|a, b| b.favorite.cmp(&a.favorite).then(b.used_at.cmp(&a.used_at)));
    list
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

impl Backend {
    pub(crate) async fn load_queries(
        &self,
        namespace: Namespace,
    ) -> Result<Vec<SavedQuery>, BackendError> {
        self.with_queries(move |store| Ok(list(&store.load()?, &namespace)))
            .await
    }

    /// Moves a query that was run before to the top instead of adding it again.
    pub(crate) async fn record_query(
        &self,
        namespace: Namespace,
        query: QueryText,
    ) -> Result<Vec<SavedQuery>, BackendError> {
        self.with_queries(move |store| {
            store.change(&namespace, |queries| {
                let now = now_millis();
                match queries
                    .iter_mut()
                    .find(|saved| saved.is_in(&namespace) && saved.query == query)
                {
                    Some(saved) => saved.used_at = now,
                    None => queries.push(SavedQuery {
                        id: Uuid::new_v4(),
                        connection: namespace.connection,
                        database: namespace.database.clone(),
                        collection: namespace.collection.clone(),
                        favorite: false,
                        used_at: now,
                        query,
                    }),
                }
                let recent = list(queries, &namespace);
                let keep: Vec<Uuid> = recent
                    .iter()
                    .filter(|saved| !saved.favorite)
                    .skip(RECENT_PER_COLLECTION)
                    .map(|saved| saved.id)
                    .collect();
                queries.retain(|saved| !keep.contains(&saved.id));
            })
        })
        .await
    }

    pub(crate) async fn set_favorite(
        &self,
        namespace: Namespace,
        id: Uuid,
        favorite: bool,
    ) -> Result<Vec<SavedQuery>, BackendError> {
        self.with_queries(move |store| {
            store.change(&namespace, |queries| {
                if let Some(saved) = queries.iter_mut().find(|saved| saved.id == id) {
                    saved.favorite = favorite;
                }
            })
        })
        .await
    }

    pub(crate) async fn delete_query(
        &self,
        namespace: Namespace,
        id: Uuid,
    ) -> Result<Vec<SavedQuery>, BackendError> {
        self.with_queries(move |store| {
            store.change(&namespace, |queries| queries.retain(|saved| saved.id != id))
        })
        .await
    }

    /// Drops all saved queries of a deleted connection.
    pub(crate) async fn forget_queries(&self, connection: Uuid) -> Result<(), BackendError> {
        self.with_queries(move |store| {
            let mut queries = store.load()?;
            queries.retain(|saved| saved.connection != connection);
            store.save(queries)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use crate::store::ConnectionStore;

    fn backend() -> Backend {
        let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
        Backend::new(
            ConnectionStore::new(dir.join("connections.toml")),
            MemorySecrets::default(),
        )
    }

    fn namespace(collection: &str) -> Namespace {
        Namespace {
            connection: Uuid::nil(),
            database: "db".to_owned(),
            collection: collection.to_owned(),
        }
    }

    fn filter(text: &str) -> QueryText {
        QueryText {
            filter: text.to_owned(),
            ..QueryText::default()
        }
    }

    fn filters(list: &[SavedQuery]) -> Vec<&str> {
        list.iter()
            .map(|saved| saved.query.filter.as_str())
            .collect()
    }

    #[tokio::test]
    async fn keeps_recent_queries_per_collection_newest_first() {
        let backend = backend();
        backend
            .record_query(namespace("a"), filter("{ x: 1 }"))
            .await
            .unwrap();
        backend
            .record_query(namespace("b"), filter("{ y: 1 }"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        backend
            .record_query(namespace("a"), filter("{ x: 2 }"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let list = backend
            .record_query(namespace("a"), filter("{ x: 1 }"))
            .await
            .unwrap();

        assert_eq!(filters(&list), ["{ x: 1 }", "{ x: 2 }"]);
        assert_eq!(
            filters(&backend.load_queries(namespace("b")).await.unwrap()),
            ["{ y: 1 }"]
        );
    }

    #[tokio::test]
    async fn favorites_survive_pruning_and_list_first() {
        let backend = backend();
        let list = backend
            .record_query(namespace("a"), filter("{ keep: 1 }"))
            .await
            .unwrap();
        backend
            .set_favorite(namespace("a"), list[0].id, true)
            .await
            .unwrap();
        for n in 0..RECENT_PER_COLLECTION + 5 {
            backend
                .record_query(namespace("a"), filter(&format!("{{ n: {n} }}")))
                .await
                .unwrap();
        }

        let list = backend.load_queries(namespace("a")).await.unwrap();

        assert_eq!(list.len(), RECENT_PER_COLLECTION + 1);
        assert_eq!(list[0].query.filter, "{ keep: 1 }");
        assert!(list[0].favorite);
    }
}
