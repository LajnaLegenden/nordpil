//! Runs mongosh-style scripts on an embedded QuickJS engine.
//!
//! A JavaScript prelude provides `db`, collections, cursors and the BSON
//! types. Every database call goes through one native function to a
//! [`Host`], which the backend implements over the driver. Top-level
//! expression statements report their values as [`Event::Result`]s.
//!
//! ```no_run
//! # fn host() -> Box<dyn nordpil_shell::Host> { unimplemented!() }
//! let summary = nordpil_shell::run("db.users.find({ age: { $gt: 30 } })", "shop", host(), |event| {
//!     println!("{event:?}");
//! });
//! ```

mod codec;
mod engine;
mod instrument;

use std::ops::Range;
use std::time::Duration;

use bson::{Bson, Document};

pub use engine::{MEMORY_LIMIT, run};

/// A collection in a database.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Namespace {
    pub database: String,
    pub collection: String,
}

/// A query a cursor runs: `find` or `aggregate`.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub namespace: Namespace,
    pub kind: QueryKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryKind {
    Find {
        filter: Document,
        options: Box<FindOptions>,
    },
    Aggregate {
        pipeline: Vec<Document>,
        options: AggregateOptions,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FindOptions {
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub skip: u64,
    /// 0 means no limit.
    pub limit: u64,
    /// An index name or key pattern.
    pub hint: Option<Bson>,
    pub collation: Option<Document>,
    /// `None` leaves it to the host's default.
    pub max_time: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AggregateOptions {
    pub allow_disk_use: Option<bool>,
    pub hint: Option<Bson>,
    pub collation: Option<Document>,
    pub max_time: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CountOptions {
    pub skip: u64,
    /// 0 means no limit.
    pub limit: u64,
    pub max_time: Option<Duration>,
}

/// One write, as the script asked for it.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Insert(Vec<Document>),
    Update {
        filter: Document,
        /// An update document or an aggregation pipeline.
        update: Bson,
        many: bool,
        options: UpdateOptions,
    },
    Replace {
        filter: Document,
        replacement: Document,
        options: UpdateOptions,
    },
    Delete {
        filter: Document,
        many: bool,
        options: DeleteOptions,
    },
    FindOneAndUpdate {
        filter: Document,
        update: Bson,
        options: FindAndModifyOptions,
    },
    FindOneAndReplace {
        filter: Document,
        replacement: Document,
        options: FindAndModifyOptions,
    },
    FindOneAndDelete {
        filter: Document,
        options: FindAndModifyOptions,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpdateOptions {
    pub upsert: bool,
    pub array_filters: Option<Vec<Document>>,
    pub collation: Option<Document>,
    pub hint: Option<Bson>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeleteOptions {
    pub collation: Option<Document>,
    pub hint: Option<Bson>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FindAndModifyOptions {
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    pub upsert: bool,
    /// Return the document after the change instead of before.
    pub return_after: bool,
    pub array_filters: Option<Vec<Document>>,
    pub collation: Option<Document>,
    pub hint: Option<Bson>,
}

/// What a write did. Fields that don't apply stay empty or 0.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriteOutcome {
    pub inserted_ids: Vec<Bson>,
    pub matched: u64,
    pub modified: u64,
    pub deleted: u64,
    pub upserted_id: Option<Bson>,
    /// The document a `findOneAnd…` found.
    pub document: Option<Document>,
}

/// The first write of a run, for the user to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteSummary {
    /// The method, e.g. `deleteMany`.
    pub operation: String,
    pub namespace: Namespace,
    /// Documents it inserts, or that match its filter now.
    pub documents: u64,
}

/// Identifies an open cursor of one run.
pub type CursorId = u64;

/// Database access for a script. Methods block until the database answers.
/// Errors are messages for the user.
pub trait Host {
    /// Checked while the script runs; true stops it.
    fn cancelled(&self) -> bool;

    /// Writes fail without asking when true.
    fn read_only(&self) -> bool;

    fn open(&mut self, query: &Query) -> Result<CursorId, String>;

    /// The next documents of a cursor; empty when it is done.
    fn next_batch(&mut self, cursor: CursorId) -> Result<Vec<Document>, String>;

    fn close(&mut self, cursor: CursorId);

    fn count(
        &mut self,
        namespace: &Namespace,
        filter: Document,
        options: CountOptions,
    ) -> Result<u64, String>;

    fn estimated_count(&mut self, namespace: &Namespace) -> Result<u64, String>;

    fn distinct(
        &mut self,
        namespace: &Namespace,
        key: &str,
        filter: Document,
    ) -> Result<Vec<Bson>, String>;

    fn write(&mut self, namespace: &Namespace, write: Write) -> Result<WriteOutcome, String>;

    /// One document per database, as `listDatabases` returns them.
    fn list_databases(&mut self) -> Result<Vec<Document>, String>;

    fn list_collections(&mut self, database: &str) -> Result<Vec<String>, String>;

    /// Asked before the first write of a run. False stops the run.
    fn confirm_write(&mut self, summary: &WriteSummary) -> bool;
}

/// What a run reports while it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A top-level statement had a value.
    Result {
        /// Index among the script's statements.
        statement: usize,
        /// Where the statement is in the script.
        span: Range<usize>,
        value: ResultValue,
        /// From the start of the statement to its value.
        elapsed: Duration,
    },
    /// A line from `print`, `printjson` or `console.log`.
    Output(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultValue {
    /// A cursor nothing read from: the query to run for its pages.
    Cursor(Box<Query>),
    /// An array of documents, e.g. from `toArray()`.
    Documents(Vec<Document>),
    Value(Bson),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Finished,
    Failed(ScriptError),
    Cancelled,
    /// The user said no to the first write.
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    pub message: String,
    /// The top-level statement that was running.
    pub statement: Option<usize>,
    /// 1-based line and column in the script, when known.
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[cfg(test)]
mod tests;
