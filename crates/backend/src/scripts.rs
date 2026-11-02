//! Shell scripts per collection, kept in `scripts.toml` next to the saved
//! connections: the draft in the editor, recent runs and favorites.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Backend, BackendError, Namespace, toml_file};

/// Older non-favorite scripts beyond this many per collection are dropped.
const RECENT_PER_COLLECTION: usize = 20;
const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedScript {
    pub id: Uuid,
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
    #[serde(default)]
    pub favorite: bool,
    /// Milliseconds since 1970.
    pub used_at: i64,
    pub script: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Draft {
    connection: Uuid,
    database: String,
    collection: String,
    text: String,
}

/// A collection's scripts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scripts {
    /// What the editor held last; `None` if never saved.
    pub draft: Option<String>,
    /// Favorites first, then the most recently run.
    pub history: Vec<SavedScript>,
}

#[derive(Default, Serialize, Deserialize)]
struct ScriptsFile {
    version: u32,
    #[serde(default, rename = "script")]
    scripts: Vec<SavedScript>,
    #[serde(default, rename = "draft")]
    drafts: Vec<Draft>,
}

fn is_in(connection: Uuid, database: &str, collection: &str, namespace: &Namespace) -> bool {
    connection == namespace.connection
        && database == namespace.database
        && collection == namespace.collection
}

impl SavedScript {
    fn is_in(&self, namespace: &Namespace) -> bool {
        is_in(self.connection, &self.database, &self.collection, namespace)
    }
}

impl Draft {
    fn is_in(&self, namespace: &Namespace) -> bool {
        is_in(self.connection, &self.database, &self.collection, namespace)
    }
}

pub struct ScriptStore {
    path: PathBuf,
}

impl ScriptStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<ScriptsFile, BackendError> {
        let Some(file) = toml_file::read::<ScriptsFile>(&self.path)? else {
            return Ok(ScriptsFile::default());
        };
        if file.version > FILE_VERSION {
            return Err(toml_file::newer_version_error(&self.path));
        }
        Ok(file)
    }

    /// Loads, changes and saves the file, then returns the collection's scripts.
    fn change(
        &self,
        namespace: &Namespace,
        change: impl FnOnce(&mut ScriptsFile),
    ) -> Result<Scripts, BackendError> {
        let mut file = self.load()?;
        change(&mut file);
        file.version = FILE_VERSION;
        let scripts = scripts(&file, namespace);
        toml_file::write(&self.path, &file)?;
        Ok(scripts)
    }
}

fn scripts(file: &ScriptsFile, namespace: &Namespace) -> Scripts {
    let mut history: Vec<_> = file
        .scripts
        .iter()
        .filter(|saved| saved.is_in(namespace))
        .cloned()
        .collect();
    history.sort_by(|a, b| b.favorite.cmp(&a.favorite).then(b.used_at.cmp(&a.used_at)));
    Scripts {
        draft: file
            .drafts
            .iter()
            .find(|draft| draft.is_in(namespace))
            .map(|draft| draft.text.clone()),
        history,
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

impl Backend {
    pub(crate) async fn load_scripts(&self, namespace: Namespace) -> Result<Scripts, BackendError> {
        self.with_scripts(move |store| Ok(scripts(&store.load()?, &namespace)))
            .await
    }

    /// Moves a script that ran before to the top instead of adding it again.
    pub(crate) async fn record_script(
        &self,
        namespace: Namespace,
        script: String,
    ) -> Result<Scripts, BackendError> {
        self.with_scripts(move |store| {
            store.change(&namespace, |file| {
                let now = now_millis();
                match file
                    .scripts
                    .iter_mut()
                    .find(|saved| saved.is_in(&namespace) && saved.script == script)
                {
                    Some(saved) => saved.used_at = now,
                    None => file.scripts.push(SavedScript {
                        id: Uuid::new_v4(),
                        connection: namespace.connection,
                        database: namespace.database.clone(),
                        collection: namespace.collection.clone(),
                        favorite: false,
                        used_at: now,
                        script,
                    }),
                }
                let drop: Vec<Uuid> = scripts(file, &namespace)
                    .history
                    .iter()
                    .filter(|saved| !saved.favorite)
                    .skip(RECENT_PER_COLLECTION)
                    .map(|saved| saved.id)
                    .collect();
                file.scripts.retain(|saved| !drop.contains(&saved.id));
            })
        })
        .await
    }

    pub(crate) async fn set_script_favorite(
        &self,
        namespace: Namespace,
        id: Uuid,
        favorite: bool,
    ) -> Result<Scripts, BackendError> {
        self.with_scripts(move |store| {
            store.change(&namespace, |file| {
                if let Some(saved) = file.scripts.iter_mut().find(|saved| saved.id == id) {
                    saved.favorite = favorite;
                }
            })
        })
        .await
    }

    pub(crate) async fn delete_script(
        &self,
        namespace: Namespace,
        id: Uuid,
    ) -> Result<Scripts, BackendError> {
        self.with_scripts(move |store| {
            store.change(&namespace, |file| {
                file.scripts.retain(|saved| saved.id != id);
            })
        })
        .await
    }

    /// An empty text forgets the draft.
    pub(crate) async fn save_draft(
        &self,
        namespace: Namespace,
        text: String,
    ) -> Result<(), BackendError> {
        self.with_scripts(move |store| {
            store
                .change(&namespace, |file| {
                    file.drafts.retain(|draft| !draft.is_in(&namespace));
                    if !text.trim().is_empty() {
                        file.drafts.push(Draft {
                            connection: namespace.connection,
                            database: namespace.database.clone(),
                            collection: namespace.collection.clone(),
                            text,
                        });
                    }
                })
                .map(|_| ())
        })
        .await
    }

    /// Drops the scripts and drafts of a deleted connection.
    pub(crate) async fn forget_scripts(&self, connection: Uuid) -> Result<(), BackendError> {
        self.with_scripts(move |store| {
            let mut file = store.load()?;
            file.scripts.retain(|saved| saved.connection != connection);
            file.drafts.retain(|draft| draft.connection != connection);
            file.version = FILE_VERSION;
            toml_file::write(&store.path, &file)
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

    #[tokio::test]
    async fn keeps_drafts_and_history_per_collection() {
        let backend = backend();
        backend
            .save_draft(namespace("a"), "db.a.find()".to_owned())
            .await
            .unwrap();
        backend
            .record_script(namespace("a"), "print(1)".to_owned())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let scripts = backend
            .record_script(namespace("a"), "print(2)".to_owned())
            .await
            .unwrap();
        backend
            .set_script_favorite(namespace("a"), scripts.history[1].id, true)
            .await
            .unwrap();

        let scripts = backend.load_scripts(namespace("a")).await.unwrap();
        assert_eq!(scripts.draft.as_deref(), Some("db.a.find()"));
        let texts: Vec<_> = scripts.history.iter().map(|s| s.script.as_str()).collect();
        assert_eq!(texts, ["print(1)", "print(2)"]);
        assert!(scripts.history[0].favorite);
        assert_eq!(
            backend.load_scripts(namespace("b")).await.unwrap(),
            Scripts::default()
        );

        backend.forget_scripts(Uuid::nil()).await.unwrap();
        assert_eq!(
            backend.load_scripts(namespace("a")).await.unwrap(),
            Scripts::default()
        );
    }
}
