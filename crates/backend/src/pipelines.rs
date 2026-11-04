//! Aggregation pipelines per collection, kept in `pipelines.toml` next to
//! the saved connections: the builder's draft, recent runs and favorites.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Backend, BackendError, Namespace, toml_file};

/// Older non-favorite pipelines beyond this many per collection are dropped.
const RECENT_PER_COLLECTION: usize = 20;
const FILE_VERSION: u32 = 1;

/// One stage as the builder holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageText {
    /// The stage's name, e.g. `$group`.
    pub name: String,
    /// Its value as typed.
    pub body: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapsed: bool,
}

fn yes() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A pipeline as the builder holds it: its stages and options as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineText {
    #[serde(default, rename = "stage")]
    pub stages: Vec<StageText>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_disk_use: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub collation: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub max_time_ms: String,
}

impl PipelineText {
    /// The same pipeline with every stage expanded, as history keeps it.
    fn expanded(mut self) -> Self {
        for stage in &mut self.stages {
            stage.collapsed = false;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPipeline {
    pub id: Uuid,
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
    #[serde(default)]
    pub favorite: bool,
    /// Milliseconds since 1970.
    pub used_at: i64,
    pub pipeline: PipelineText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Draft {
    connection: Uuid,
    database: String,
    collection: String,
    pipeline: PipelineText,
}

/// A collection's pipelines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pipelines {
    /// What the builder held last; `None` if never saved.
    pub draft: Option<PipelineText>,
    /// Favorites first, then the most recently run.
    pub history: Vec<SavedPipeline>,
}

#[derive(Default, Serialize, Deserialize)]
struct PipelinesFile {
    version: u32,
    #[serde(default, rename = "pipeline")]
    pipelines: Vec<SavedPipeline>,
    #[serde(default, rename = "draft")]
    drafts: Vec<Draft>,
}

fn is_in(connection: Uuid, database: &str, collection: &str, namespace: &Namespace) -> bool {
    connection == namespace.connection
        && database == namespace.database
        && collection == namespace.collection
}

impl SavedPipeline {
    fn is_in(&self, namespace: &Namespace) -> bool {
        is_in(self.connection, &self.database, &self.collection, namespace)
    }
}

impl Draft {
    fn is_in(&self, namespace: &Namespace) -> bool {
        is_in(self.connection, &self.database, &self.collection, namespace)
    }
}

pub struct PipelineStore {
    path: PathBuf,
}

impl PipelineStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<PipelinesFile, BackendError> {
        let Some(file) = toml_file::read::<PipelinesFile>(&self.path)? else {
            return Ok(PipelinesFile::default());
        };
        if file.version > FILE_VERSION {
            return Err(toml_file::newer_version_error(&self.path));
        }
        Ok(file)
    }

    /// Loads, changes and saves the file, then returns the collection's pipelines.
    fn change(
        &self,
        namespace: &Namespace,
        change: impl FnOnce(&mut PipelinesFile),
    ) -> Result<Pipelines, BackendError> {
        let mut file = self.load()?;
        change(&mut file);
        file.version = FILE_VERSION;
        let pipelines = pipelines(&file, namespace);
        toml_file::write(&self.path, &file)?;
        Ok(pipelines)
    }
}

fn pipelines(file: &PipelinesFile, namespace: &Namespace) -> Pipelines {
    let mut history: Vec<_> = file
        .pipelines
        .iter()
        .filter(|saved| saved.is_in(namespace))
        .cloned()
        .collect();
    history.sort_by(|a, b| b.favorite.cmp(&a.favorite).then(b.used_at.cmp(&a.used_at)));
    Pipelines {
        draft: file
            .drafts
            .iter()
            .find(|draft| draft.is_in(namespace))
            .map(|draft| draft.pipeline.clone()),
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
    pub(crate) async fn load_pipelines(
        &self,
        namespace: Namespace,
    ) -> Result<Pipelines, BackendError> {
        self.with_pipelines(move |store| Ok(pipelines(&store.load()?, &namespace)))
            .await
    }

    /// Moves a pipeline that ran before to the top instead of adding it again.
    pub(crate) async fn record_pipeline(
        &self,
        namespace: Namespace,
        pipeline: PipelineText,
    ) -> Result<Pipelines, BackendError> {
        let pipeline = pipeline.expanded();
        self.with_pipelines(move |store| {
            store.change(&namespace, |file| {
                let now = now_millis();
                match file
                    .pipelines
                    .iter_mut()
                    .find(|saved| saved.is_in(&namespace) && saved.pipeline == pipeline)
                {
                    Some(saved) => saved.used_at = now,
                    None => file.pipelines.push(SavedPipeline {
                        id: Uuid::new_v4(),
                        connection: namespace.connection,
                        database: namespace.database.clone(),
                        collection: namespace.collection.clone(),
                        favorite: false,
                        used_at: now,
                        pipeline,
                    }),
                }
                let drop: Vec<Uuid> = pipelines(file, &namespace)
                    .history
                    .iter()
                    .filter(|saved| !saved.favorite)
                    .skip(RECENT_PER_COLLECTION)
                    .map(|saved| saved.id)
                    .collect();
                file.pipelines.retain(|saved| !drop.contains(&saved.id));
            })
        })
        .await
    }

    pub(crate) async fn set_pipeline_favorite(
        &self,
        namespace: Namespace,
        id: Uuid,
        favorite: bool,
    ) -> Result<Pipelines, BackendError> {
        self.with_pipelines(move |store| {
            store.change(&namespace, |file| {
                if let Some(saved) = file.pipelines.iter_mut().find(|saved| saved.id == id) {
                    saved.favorite = favorite;
                }
            })
        })
        .await
    }

    pub(crate) async fn delete_pipeline(
        &self,
        namespace: Namespace,
        id: Uuid,
    ) -> Result<Pipelines, BackendError> {
        self.with_pipelines(move |store| {
            store.change(&namespace, |file| {
                file.pipelines.retain(|saved| saved.id != id);
            })
        })
        .await
    }

    /// A pipeline without stages or options forgets the draft.
    pub(crate) async fn save_pipeline_draft(
        &self,
        namespace: Namespace,
        pipeline: PipelineText,
    ) -> Result<(), BackendError> {
        self.with_pipelines(move |store| {
            store
                .change(&namespace, |file| {
                    file.drafts.retain(|draft| !draft.is_in(&namespace));
                    if pipeline != PipelineText::default() {
                        file.drafts.push(Draft {
                            connection: namespace.connection,
                            database: namespace.database.clone(),
                            collection: namespace.collection.clone(),
                            pipeline,
                        });
                    }
                })
                .map(|_| ())
        })
        .await
    }

    /// Drops the pipelines and drafts of a deleted connection.
    pub(crate) async fn forget_pipelines(&self, connection: Uuid) -> Result<(), BackendError> {
        self.with_pipelines(move |store| {
            let mut file = store.load()?;
            file.pipelines
                .retain(|saved| saved.connection != connection);
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

    fn pipeline(stages: &[(&str, &str)]) -> PipelineText {
        PipelineText {
            stages: stages
                .iter()
                .map(|&(name, body)| StageText {
                    name: name.to_owned(),
                    body: body.to_owned(),
                    enabled: true,
                    collapsed: false,
                })
                .collect(),
            ..PipelineText::default()
        }
    }

    #[tokio::test]
    async fn keeps_drafts_and_history_per_collection() {
        let backend = backend();
        let mut draft = pipeline(&[("$match", "{\n  a: 'x' // first\n}"), ("$limit", "5")]);
        draft.stages[1].enabled = false;
        draft.stages[0].collapsed = true;
        draft.hint = "{ a: 1 }".to_owned();
        backend
            .save_pipeline_draft(namespace("a"), draft.clone())
            .await
            .unwrap();
        backend
            .record_pipeline(namespace("a"), pipeline(&[("$limit", "1")]))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let pipelines = backend
            .record_pipeline(namespace("a"), draft.clone())
            .await
            .unwrap();
        backend
            .set_pipeline_favorite(namespace("a"), pipelines.history[1].id, true)
            .await
            .unwrap();

        let pipelines = backend.load_pipelines(namespace("a")).await.unwrap();
        assert_eq!(pipelines.draft.as_ref(), Some(&draft));
        assert_eq!(pipelines.history[0].pipeline, pipeline(&[("$limit", "1")]));
        assert!(pipelines.history[0].favorite);
        // History keeps stages expanded.
        assert_eq!(pipelines.history[1].pipeline, draft.clone().expanded());
        assert_eq!(
            backend.load_pipelines(namespace("b")).await.unwrap(),
            Pipelines::default()
        );

        backend
            .save_pipeline_draft(namespace("a"), PipelineText::default())
            .await
            .unwrap();
        assert_eq!(
            backend.load_pipelines(namespace("a")).await.unwrap().draft,
            None
        );
        backend.forget_pipelines(Uuid::nil()).await.unwrap();
        assert_eq!(
            backend.load_pipelines(namespace("a")).await.unwrap(),
            Pipelines::default()
        );
    }
}
