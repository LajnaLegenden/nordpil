//! Runs shell scripts: the driver behind `nordpil_shell::Host`, on a
//! blocking thread, with results and write confirmations shared with the UI.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures_util::TryStreamExt as _;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{
    AggregateOptions, CountOptions, DeleteOptions, FindOneAndDeleteOptions,
    FindOneAndReplaceOptions, FindOneAndUpdateOptions, FindOptions, ReplaceOptions, ReturnDocument,
    UpdateModifications, UpdateOptions,
};
use mongodb::{Client, Cursor};
use nordpil_shell::{CursorId, Host, Outcome, Query, QueryKind, Write, WriteOutcome, WriteSummary};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::documents::{collation, to_hint};
use crate::{Backend, BackendError, FoundDocument, Namespace};

/// Like the Documents panel, script queries without a time limit get one.
pub const SCRIPT_MAX_TIME: Duration = Duration::from_secs(60);
/// Documents a cursor hands the script at a time.
const BATCH: usize = 256;

/// One run's results and write confirmation, shared between the script's
/// thread and the UI, which polls it each frame.
#[derive(Debug)]
pub struct ScriptRun {
    events: Mutex<Vec<nordpil_shell::Event>>,
    pending_write: Mutex<Option<WriteSummary>>,
    answers: (Sender<bool>, Receiver<bool>),
}

impl ScriptRun {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::default(),
            pending_write: Mutex::default(),
            answers: crossbeam_channel::bounded(1),
        })
    }

    /// Events since the last call.
    pub fn take_events(&self) -> Vec<nordpil_shell::Event> {
        std::mem::take(&mut *lock(&self.events))
    }

    /// The write waiting for the user's answer.
    pub fn pending_write(&self) -> Option<WriteSummary> {
        lock(&self.pending_write).clone()
    }

    pub fn answer(&self, allow: bool) {
        if lock(&self.pending_write).take().is_some() {
            let _ = self.answers.0.try_send(allow);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No code panics while holding these locks.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An aggregation to page through, from a shell result.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateQuery {
    pub pipeline: Vec<Document>,
    pub options: nordpil_shell::AggregateOptions,
    pub skip: u64,
    /// 0 means no limit.
    pub limit: u64,
}

impl Backend {
    pub(crate) async fn run_script(
        &self,
        namespace: &Namespace,
        script: String,
        run: Arc<ScriptRun>,
        cancel: CancellationToken,
    ) -> Result<Outcome, BackendError> {
        let session = self.session(namespace.connection)?;
        let database = namespace.database.clone();
        let handle = Handle::current();
        crate::blocking(move || {
            let host = DriverHost {
                client: session.client,
                read_only: session.read_only,
                handle,
                cancel,
                run: Arc::clone(&run),
                cursors: HashMap::new(),
                next_id: 0,
            };
            let sink = move |event| lock(&run.events).push(event);
            Ok(nordpil_shell::run(&script, &database, Box::new(host), sink))
        })
        .await
    }

    pub(crate) async fn aggregate(
        &self,
        namespace: &Namespace,
        query: AggregateQuery,
    ) -> Result<Vec<FoundDocument>, BackendError> {
        let client = self.session(namespace.connection)?.client;
        let mut pipeline = query.pipeline;
        if query.skip > 0 {
            pipeline.push(doc! { "$skip": to_i64(query.skip) });
        }
        if query.limit > 0 {
            pipeline.push(doc! { "$limit": to_i64(query.limit) });
        }
        let options = aggregate_options(query.options)?;
        let documents: Vec<Document> = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .aggregate(pipeline)
            .with_options(options)
            .await?
            .try_collect()
            .await?;
        crate::blocking(move || Ok(documents.into_iter().map(FoundDocument::new).collect())).await
    }
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn aggregate_options(
    options: nordpil_shell::AggregateOptions,
) -> Result<AggregateOptions, BackendError> {
    let mut driver = AggregateOptions::default();
    driver.allow_disk_use = options.allow_disk_use;
    driver.hint = options.hint.map(to_hint).transpose()?;
    driver.collation = options.collation.map(collation).transpose()?;
    driver.max_time = Some(options.max_time.unwrap_or(SCRIPT_MAX_TIME));
    Ok(driver)
}

/// The driver behind a script. Runs on a blocking thread and waits for
/// each database call on the backend runtime.
struct DriverHost {
    client: Client,
    read_only: bool,
    handle: Handle,
    cancel: CancellationToken,
    run: Arc<ScriptRun>,
    cursors: HashMap<CursorId, Cursor<Document>>,
    next_id: CursorId,
}

impl DriverHost {
    fn collection(&self, namespace: &nordpil_shell::Namespace) -> mongodb::Collection<Document> {
        self.client
            .database(&namespace.database)
            .collection(&namespace.collection)
    }

    /// Waits for `future`, giving up when the run is stopped.
    fn wait<T>(&self, future: impl Future<Output = Result<T, BackendError>>) -> Result<T, String> {
        let cancel = self.cancel.clone();
        self.handle.block_on(async move {
            tokio::select! {
                () = cancel.cancelled() => Err("Cancelled.".to_owned()),
                result = future => result.map_err(|error| error.message),
            }
        })
    }
}

impl Host for DriverHost {
    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    fn open(&mut self, query: &Query) -> Result<CursorId, String> {
        let collection = self.collection(&query.namespace);
        let cursor = match &query.kind {
            QueryKind::Find { filter, options } => {
                let options = find_options(options).map_err(|error| error.message)?;
                let filter = filter.clone();
                self.wait(async move { Ok(collection.find(filter).with_options(options).await?) })?
            }
            QueryKind::Aggregate { pipeline, options } => {
                let options = aggregate_options(options.clone()).map_err(|error| error.message)?;
                let pipeline = pipeline.clone();
                self.wait(async move {
                    Ok(collection.aggregate(pipeline).with_options(options).await?)
                })?
            }
        };
        self.next_id += 1;
        self.cursors.insert(self.next_id, cursor);
        Ok(self.next_id)
    }

    fn next_batch(&mut self, cursor: CursorId) -> Result<Vec<Document>, String> {
        let mut open = self
            .cursors
            .remove(&cursor)
            .ok_or_else(|| "This cursor is closed.".to_owned())?;
        let batch = self.wait(async {
            let mut batch = Vec::new();
            while batch.len() < BATCH
                && let Some(document) = open.try_next().await?
            {
                batch.push(document);
            }
            Ok(batch)
        })?;
        if !batch.is_empty() {
            self.cursors.insert(cursor, open);
        } else {
            let _runtime = self.handle.enter();
            drop(open);
        }
        Ok(batch)
    }

    fn close(&mut self, cursor: CursorId) {
        // Dropping a cursor kills it on the server from a runtime task.
        let _runtime = self.handle.enter();
        self.cursors.remove(&cursor);
    }

    fn count(
        &mut self,
        namespace: &nordpil_shell::Namespace,
        filter: Document,
        options: nordpil_shell::CountOptions,
    ) -> Result<u64, String> {
        let collection = self.collection(namespace);
        let mut driver = CountOptions::default();
        driver.skip = (options.skip > 0).then_some(options.skip);
        driver.limit = (options.limit > 0).then_some(options.limit);
        driver.max_time = Some(options.max_time.unwrap_or(SCRIPT_MAX_TIME));
        self.wait(async move {
            Ok(collection
                .count_documents(filter)
                .with_options(driver)
                .await?)
        })
    }

    fn estimated_count(&mut self, namespace: &nordpil_shell::Namespace) -> Result<u64, String> {
        let collection = self.collection(namespace);
        self.wait(async move { Ok(collection.estimated_document_count().await?) })
    }

    fn distinct(
        &mut self,
        namespace: &nordpil_shell::Namespace,
        key: &str,
        filter: Document,
    ) -> Result<Vec<Bson>, String> {
        let collection = self.collection(namespace);
        let key = key.to_owned();
        self.wait(async move {
            Ok(collection
                .distinct(key, filter)
                .max_time(SCRIPT_MAX_TIME)
                .await?)
        })
    }

    fn write(
        &mut self,
        namespace: &nordpil_shell::Namespace,
        write: Write,
    ) -> Result<WriteOutcome, String> {
        if self.read_only {
            return Err(BackendError::read_only().message);
        }
        let collection = self.collection(namespace);
        self.wait(async move { write_with(&collection, write).await })
    }

    fn list_databases(&mut self) -> Result<Vec<Document>, String> {
        let client = self.client.clone();
        let databases = self.wait(async move { crate::databases::list(&client).await })?;
        Ok(databases
            .into_iter()
            .map(|database| {
                doc! {
                    "name": database.name,
                    "sizeOnDisk": to_i64(database.size_on_disk),
                    "empty": database.empty,
                }
            })
            .collect())
    }

    fn list_collections(&mut self, database: &str) -> Result<Vec<String>, String> {
        let database = self.client.database(database);
        let mut names = self.wait(async move { Ok(database.list_collection_names().await?) })?;
        names.sort();
        Ok(names)
    }

    fn confirm_write(&mut self, summary: &WriteSummary) -> bool {
        // Drop an answer left from an earlier question.
        while self.run.answers.1.try_recv().is_ok() {}
        *lock(&self.run.pending_write) = Some(summary.clone());
        loop {
            match self.run.answers.1.recv_timeout(Duration::from_millis(100)) {
                Ok(allow) => return allow,
                Err(_) if self.cancel.is_cancelled() => {
                    lock(&self.run.pending_write).take();
                    return false;
                }
                Err(_) => {}
            }
        }
    }
}

fn find_options(options: &nordpil_shell::FindOptions) -> Result<FindOptions, BackendError> {
    let mut driver = FindOptions::default();
    driver.projection.clone_from(&options.projection);
    driver.sort.clone_from(&options.sort);
    driver.skip = (options.skip > 0).then_some(options.skip);
    driver.limit = (options.limit > 0).then_some(to_i64(options.limit));
    driver.hint = options.hint.clone().map(to_hint).transpose()?;
    driver.collation = options.collation.clone().map(collation).transpose()?;
    driver.max_time = Some(options.max_time.unwrap_or(SCRIPT_MAX_TIME));
    Ok(driver)
}

fn modifications(update: Bson) -> Result<UpdateModifications, BackendError> {
    match update {
        Bson::Document(update) => Ok(UpdateModifications::Document(update)),
        Bson::Array(stages) => stages
            .into_iter()
            .map(|stage| match stage {
                Bson::Document(stage) => Ok(stage),
                _ => Err(BackendError::new("Each pipeline stage must be a document.")),
            })
            .collect::<Result<_, _>>()
            .map(UpdateModifications::Pipeline),
        _ => Err(BackendError::new(
            "Use update operators, e.g. { $set: { field: 'value' } }, or a pipeline.",
        )),
    }
}

async fn write_with(
    collection: &mongodb::Collection<Document>,
    write: Write,
) -> Result<WriteOutcome, BackendError> {
    Ok(match write {
        Write::Insert(documents) => {
            let result = collection.insert_many(documents).await?;
            let mut ids: Vec<(usize, Bson)> = result.inserted_ids.into_iter().collect();
            ids.sort_by_key(|(index, _)| *index);
            WriteOutcome {
                inserted_ids: ids.into_iter().map(|(_, id)| id).collect(),
                ..WriteOutcome::default()
            }
        }
        Write::Update {
            filter,
            update,
            many,
            options,
        } => {
            let mut driver = UpdateOptions::default();
            driver.upsert = Some(options.upsert);
            driver.array_filters = options.array_filters;
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let update = modifications(update)?;
            let result = if many {
                collection
                    .update_many(filter, update)
                    .with_options(driver)
                    .await?
            } else {
                collection
                    .update_one(filter, update)
                    .with_options(driver)
                    .await?
            };
            WriteOutcome {
                matched: result.matched_count,
                modified: result.modified_count,
                upserted_id: result.upserted_id,
                ..WriteOutcome::default()
            }
        }
        Write::Replace {
            filter,
            replacement,
            options,
        } => {
            let mut driver = ReplaceOptions::default();
            driver.upsert = Some(options.upsert);
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let result = collection
                .replace_one(filter, replacement)
                .with_options(driver)
                .await?;
            WriteOutcome {
                matched: result.matched_count,
                modified: result.modified_count,
                upserted_id: result.upserted_id,
                ..WriteOutcome::default()
            }
        }
        Write::Delete {
            filter,
            many,
            options,
        } => {
            let mut driver = DeleteOptions::default();
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let result = if many {
                collection.delete_many(filter).with_options(driver).await?
            } else {
                collection.delete_one(filter).with_options(driver).await?
            };
            WriteOutcome {
                deleted: result.deleted_count,
                ..WriteOutcome::default()
            }
        }
        Write::FindOneAndUpdate {
            filter,
            update,
            options,
        } => {
            let mut driver = FindOneAndUpdateOptions::default();
            driver.sort = options.sort;
            driver.projection = options.projection;
            driver.upsert = Some(options.upsert);
            driver.return_document = Some(return_document(options.return_after));
            driver.array_filters = options.array_filters;
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let document = collection
                .find_one_and_update(filter, modifications(update)?)
                .with_options(driver)
                .await?;
            WriteOutcome {
                document,
                ..WriteOutcome::default()
            }
        }
        Write::FindOneAndReplace {
            filter,
            replacement,
            options,
        } => {
            let mut driver = FindOneAndReplaceOptions::default();
            driver.sort = options.sort;
            driver.projection = options.projection;
            driver.upsert = Some(options.upsert);
            driver.return_document = Some(return_document(options.return_after));
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let document = collection
                .find_one_and_replace(filter, replacement)
                .with_options(driver)
                .await?;
            WriteOutcome {
                document,
                ..WriteOutcome::default()
            }
        }
        Write::FindOneAndDelete { filter, options } => {
            let mut driver = FindOneAndDeleteOptions::default();
            driver.sort = options.sort;
            driver.projection = options.projection;
            driver.collation = options.collation.map(collation).transpose()?;
            driver.hint = options.hint.map(to_hint).transpose()?;
            let document = collection
                .find_one_and_delete(filter)
                .with_options(driver)
                .await?;
            WriteOutcome {
                document,
                ..WriteOutcome::default()
            }
        }
    })
}

fn return_document(after: bool) -> ReturnDocument {
    if after {
        ReturnDocument::After
    } else {
        ReturnDocument::Before
    }
}
