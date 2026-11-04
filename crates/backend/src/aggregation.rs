//! The aggregation panel's queries: results paged from a server cursor that
//! stays open between pages, counts, explain, and pipelines that write.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::TryStreamExt as _;
use mongodb::Cursor;
use mongodb::bson::{Bson, Document, doc};
use uuid::Uuid;

use crate::documents::{COUNT_TIME_LIMIT, server_code};
use crate::shell::aggregate_options;
use crate::{Backend, BackendError, Namespace};

/// Server error code when a cursor is gone, e.g. after 10 idle minutes.
const CURSOR_NOT_FOUND: i32 = 43;

/// Names one run's cursor. The app picks a new one for every run.
pub type CursorKey = u64;

/// A pipeline and its options, as the aggregation panel runs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PipelineQuery {
    pub stages: Vec<Document>,
    pub options: nordpil_shell::AggregateOptions,
}

/// Documents from a run, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatePage {
    pub documents: Vec<Document>,
    /// The run has no documents after these.
    pub exhausted: bool,
}

/// A run's server cursor, between pages.
struct OpenCursor {
    connection: Uuid,
    cursor: Cursor<Document>,
    /// Documents the run handed out so far.
    offset: u64,
}

/// The open cursors, by key.
#[derive(Default)]
pub(crate) struct Cursors(Mutex<HashMap<CursorKey, OpenCursor>>);

impl Cursors {
    fn lock(&self) -> MutexGuard<'_, HashMap<CursorKey, OpenCursor>> {
        // No code panics while holding the lock.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Closes the cursor; dropping it kills it on the server.
    pub(crate) fn close(&self, key: CursorKey) {
        let cursor = self.lock().remove(&key);
        drop(cursor);
    }

    /// Closes every cursor of a connection.
    pub(crate) fn close_connection(&self, connection: Uuid) {
        let closed: Vec<OpenCursor> = {
            let mut cursors = self.lock();
            let keys: Vec<CursorKey> = cursors
                .iter()
                .filter(|(_, open)| open.connection == connection)
                .map(|(key, _)| *key)
                .collect();
            keys.iter().filter_map(|key| cursors.remove(key)).collect()
        };
        drop(closed);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

impl Backend {
    /// `count` documents of the run from `offset` on. Continues the run's
    /// open cursor when it stopped at `offset`, and otherwise runs the
    /// pipeline again from there: on the first page, after a stop, or when
    /// the server dropped an idle cursor.
    pub(crate) async fn aggregate_page(
        &self,
        namespace: &Namespace,
        key: CursorKey,
        query: PipelineQuery,
        offset: u64,
        count: u64,
    ) -> Result<AggregatePage, BackendError> {
        refuse_writes(&query.stages)?;
        // Taken out while it reads, so a cancelled read drops it, which ends
        // the read on the server and kills the cursor.
        let open = self
            .cursors
            .lock()
            .remove(&key)
            .filter(|open| open.offset == offset);
        let (mut open, resumed) = match open {
            Some(open) => (open, true),
            None => (self.open(namespace, &query, offset, count).await?, false),
        };
        let documents = match read(&mut open.cursor, count).await {
            Err(error) if resumed && server_code(&error) == Some(CURSOR_NOT_FOUND) => {
                open = self.open(namespace, &query, offset, count).await?;
                read(&mut open.cursor, count).await?
            }
            result => result?,
        };
        open.offset += documents.len() as u64;
        let exhausted = !open.cursor.has_next();
        if !exhausted {
            self.cursors.lock().insert(key, open);
        }
        Ok(AggregatePage {
            documents,
            exhausted,
        })
    }

    async fn open(
        &self,
        namespace: &Namespace,
        query: &PipelineQuery,
        skip: u64,
        batch: u64,
    ) -> Result<OpenCursor, BackendError> {
        let session = self.session(namespace.connection)?;
        let mut stages = query.stages.clone();
        if skip > 0 {
            stages.push(doc! { "$skip": to_i64(skip) });
        }
        let mut options = aggregate_options(query.options.clone())?;
        options.batch_size = Some(u32::try_from(batch.max(1)).unwrap_or(u32::MAX));
        let cursor = session
            .client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .aggregate(stages)
            .with_options(options)
            .await?;
        Ok(OpenCursor {
            connection: namespace.connection,
            cursor,
            offset: skip,
        })
    }

    /// How many documents the pipeline gives; `None` when counting took too
    /// long. Pipelines that write aren't counted.
    pub(crate) async fn aggregate_count(
        &self,
        namespace: &Namespace,
        query: PipelineQuery,
    ) -> Result<Option<u64>, BackendError> {
        refuse_writes(&query.stages)?;
        if query.stages.is_empty() && query.options.hint.is_none() {
            return self.count(namespace, Document::new(), None).await;
        }
        let client = self.session(namespace.connection)?.client;
        let mut stages = query.stages;
        stages.push(doc! { "$count": "n" });
        let mut options = aggregate_options(query.options)?;
        options.max_time = Some(COUNT_TIME_LIMIT);
        let result = async {
            client
                .database(&namespace.database)
                .collection::<Document>(&namespace.collection)
                .aggregate(stages)
                .with_options(options)
                .await?
                .try_next()
                .await
        }
        .await;
        match result {
            Ok(Some(counted)) => Ok(Some(counted_n(&counted))),
            Ok(None) => Ok(Some(0)),
            Err(error) if server_code(&error) == Some(crate::documents::MAX_TIME_EXPIRED) => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The server's query plan for the pipeline.
    pub(crate) async fn explain_aggregate(
        &self,
        namespace: &Namespace,
        query: PipelineQuery,
    ) -> Result<Document, BackendError> {
        refuse_writes(&query.stages)?;
        let client = self.session(namespace.connection)?.client;
        let mut aggregate = doc! {
            "aggregate": &namespace.collection,
            "pipeline": query.stages,
            "cursor": {},
        };
        let options = query.options;
        if let Some(allow) = options.allow_disk_use {
            aggregate.insert("allowDiskUse", allow);
        }
        if let Some(hint) = options.hint {
            aggregate.insert("hint", hint);
        }
        if let Some(collation) = options.collation {
            aggregate.insert("collation", collation);
        }
        let mut command = doc! { "explain": aggregate, "verbosity": "queryPlanner" };
        let max_time = options.max_time.unwrap_or(crate::SCRIPT_MAX_TIME);
        command.insert("maxTimeMS", to_i64(max_time.as_millis() as u64));
        Ok(client
            .database(&namespace.database)
            .run_command(command)
            .await?)
    }

    /// Runs a pipeline that ends in `$out` or `$merge` to the end.
    pub(crate) async fn aggregate_write(
        &self,
        namespace: &Namespace,
        query: PipelineQuery,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        let options = aggregate_options(query.options)?;
        let mut cursor = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .aggregate(query.stages)
            .with_options(options)
            .await?;
        while cursor.try_next().await?.is_some() {}
        Ok(())
    }
}

/// Paging, counting or explaining would write, so writing pipelines only run
/// through [`Backend::aggregate_write`].
fn refuse_writes(stages: &[Document]) -> Result<(), BackendError> {
    let writes = stages.iter().any(|stage| {
        stage
            .keys()
            .any(|name| nordpil_query::pipeline::is_write(name))
    });
    if writes {
        return Err(BackendError::new(
            "This pipeline writes with $out or $merge; run it with Run.",
        ));
    }
    Ok(())
}

async fn read(cursor: &mut Cursor<Document>, count: u64) -> mongodb::error::Result<Vec<Document>> {
    let mut documents = Vec::new();
    while (documents.len() as u64) < count {
        match cursor.try_next().await? {
            Some(document) => documents.push(document),
            None => break,
        }
    }
    Ok(documents)
}

fn counted_n(counted: &Document) -> u64 {
    match counted.get("n") {
        Some(Bson::Int32(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Bson::Int64(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Bson::Double(n)) => *n as u64,
        _ => 0,
    }
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// How long previews of a stage's output may take.
pub const PREVIEW_TIME_LIMIT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_page_through_writes() {
        assert!(refuse_writes(&[doc! { "$match": {} }]).is_ok());
        let error = refuse_writes(&[doc! { "$match": {} }, doc! { "$out": "copy" }]).unwrap_err();
        assert!(error.message.contains("$out or $merge"));
    }

    #[test]
    fn reads_the_count_in_any_number_type() {
        assert_eq!(counted_n(&doc! { "n": 3 }), 3);
        assert_eq!(counted_n(&doc! { "n": 3_i64 }), 3);
        assert_eq!(counted_n(&doc! {}), 0);
    }

    #[test]
    fn starts_empty() {
        let cursors = Cursors::default();
        cursors.close(1);
        cursors.close_connection(Uuid::new_v4());
        assert_eq!(cursors.len(), 0);
    }
}
