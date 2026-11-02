//! One run: a fresh QuickJS runtime, the prelude, and the native `__host`
//! that turns the prelude's calls into [`Host`] calls.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bson::oid::ObjectId;
use bson::{Bson, Document, doc};
use nordpil_query::{Style, format_value};
use rquickjs::context::EvalOptions;
use rquickjs::convert::Coerced;
use rquickjs::{Context, Ctx, Exception, Function, Runtime};

use crate::instrument::{Instrumented, instrument};
use crate::{
    AggregateOptions, CountOptions, CursorId, DeleteOptions, Event, FindAndModifyOptions,
    FindOptions, Host, Namespace, Outcome, Query, QueryKind, ResultValue, ScriptError,
    UpdateOptions, Write, WriteOutcome, WriteSummary, codec,
};

/// The most memory a run's JavaScript may use.
pub const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
const MAX_STACK: usize = 1024 * 1024;
const PRELUDE: &str = include_str!("prelude.js");
/// The script's file name in error stacks.
const FILE_NAME: &str = "script";

struct State {
    host: Box<dyn Host>,
    sink: Box<dyn FnMut(Event)>,
    statements: Vec<Range<usize>>,
    /// The top-level statement running now.
    current: Option<usize>,
    started: Instant,
    /// The user allowed this run's writes.
    confirmed: bool,
    /// The user refused a write; the run stops.
    declined: bool,
    open_cursors: Vec<CursorId>,
}

/// Runs `script` against `database`, sending results and output to `sink`
/// as they come. Blocks until the script ends.
pub fn run(
    script: &str,
    database: &str,
    host: Box<dyn Host>,
    sink: impl FnMut(Event) + 'static,
) -> Outcome {
    let instrumented = instrument(script);
    let state = Rc::new(RefCell::new(State {
        host,
        sink: Box::new(sink),
        statements: instrumented
            .statements
            .iter()
            .map(|statement| statement.span.clone())
            .collect(),
        current: None,
        started: Instant::now(),
        confirmed: false,
        declined: false,
        open_cursors: Vec::new(),
    }));
    let result = execute(&instrumented, database, &state);
    let mut state = state.borrow_mut();
    for cursor in std::mem::take(&mut state.open_cursors) {
        state.host.close(cursor);
    }
    if state.declined {
        return Outcome::Declined;
    }
    if state.host.cancelled() {
        return Outcome::Cancelled;
    }
    match result {
        Ok(()) => Outcome::Finished,
        Err(thrown) => {
            let position = thrown
                .position
                .map(|(line, column)| instrumented.original_position(script, line, column));
            Outcome::Failed(ScriptError {
                message: thrown.message,
                statement: state.current,
                line: position.map(|(line, _)| line),
                column: position.map(|(_, column)| column),
            })
        }
    }
}

/// What the script threw, with its line and column in the run code.
struct Thrown {
    message: String,
    position: Option<(usize, usize)>,
}

fn execute(
    instrumented: &Instrumented,
    database: &str,
    state: &Rc<RefCell<State>>,
) -> Result<(), Thrown> {
    let internal = |error: rquickjs::Error| Thrown {
        message: format!("The script engine failed: {error}"),
        position: None,
    };
    let runtime = Runtime::new().map_err(internal)?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(MAX_STACK);
    let interrupt = Rc::clone(state);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt
            .try_borrow()
            .is_ok_and(|state| state.declined || state.host.cancelled())
    })));
    let context = Context::full(&runtime).map_err(internal)?;
    context.with(|ctx| {
        install(&ctx, state).map_err(internal)?;
        let database = serde_json::Value::String(database.to_owned());
        let setup = format!("{PRELUDE}\nglobalThis.db = __database({database});");
        ctx.eval::<(), _>(setup)
            .map_err(|error| caught(&ctx, error))?;
        let mut options = EvalOptions::default();
        options.strict = false;
        options.filename = Some(FILE_NAME.to_owned());
        ctx.eval_with_options::<(), _>(instrumented.code.as_str(), options)
            .map_err(|error| caught(&ctx, error))
    })
}

fn install<'js>(ctx: &Ctx<'js>, state: &Rc<RefCell<State>>) -> rquickjs::Result<()> {
    let host_state = Rc::clone(state);
    let host = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, op: String, payload: String| -> rquickjs::Result<String> {
            dispatch(&host_state, &op, &payload)
                .map_err(|message| Exception::throw_message(&ctx, &message))
        },
    )?;
    let at_state = Rc::clone(state);
    let at = Function::new(ctx.clone(), move |index: usize| {
        let mut state = at_state.borrow_mut();
        state.current = Some(index);
        state.started = Instant::now();
    })?;
    ctx.globals().set("__host", host)?;
    ctx.globals().set("__at", at)?;
    Ok(())
}

/// The message and position of what the script threw.
fn caught(ctx: &Ctx<'_>, error: rquickjs::Error) -> Thrown {
    if !matches!(error, rquickjs::Error::Exception) {
        return Thrown {
            message: error.to_string(),
            position: None,
        };
    }
    let value = ctx.catch();
    let Some(object) = value.as_object() else {
        let text = value
            .get::<Coerced<String>>()
            .map_or_else(|_| "Unknown error".to_owned(), |text| text.0);
        return Thrown {
            message: format!("Uncaught {text}"),
            position: None,
        };
    };
    let text = |key: &str| {
        object
            .get::<_, Option<Coerced<String>>>(key)
            .ok()
            .flatten()
            .map(|text| text.0)
            .unwrap_or_default()
    };
    let (name, message, stack) = (text("name"), text("message"), text("stack"));
    let message = match (name.as_str(), message.as_str()) {
        ("InternalError", "out of memory") => format!(
            "The script used more than {} MB. Read big results with forEach() or limit() \
             instead of toArray().",
            MEMORY_LIMIT / (1024 * 1024)
        ),
        ("" | "Error", message) => message.to_owned(),
        (name, message) => format!("{name}: {message}"),
    };
    Thrown {
        message,
        position: stack_position(&stack),
    }
}

/// The first `script:line:column` in an error stack.
fn stack_position(stack: &str) -> Option<(usize, usize)> {
    let marker = format!("{FILE_NAME}:");
    let start = stack.find(&marker)? + marker.len();
    let mut parts = stack[start..]
        .split(|c: char| !c.is_ascii_digit())
        .take(2)
        .map(|part| part.parse::<usize>().ok());
    let line = parts.next()??;
    let column = parts.next().flatten().unwrap_or(1);
    Some((line, column))
}

fn dispatch(state: &RefCell<State>, op: &str, payload: &str) -> Result<String, String> {
    let json = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let input = codec::from_json(json)?;
    let mut state = state
        .try_borrow_mut()
        .map_err(|_| "The shell can't call the database from here.".to_owned())?;
    if state.declined || state.host.cancelled() {
        return Err("Cancelled.".to_owned());
    }
    let output = state.handle(op, input)?;
    Ok(codec::to_json(output).to_string())
}

impl State {
    fn handle(&mut self, op: &str, input: Bson) -> Result<Bson, String> {
        Ok(match op {
            "objectId" => Bson::String(ObjectId::new().to_hex()),
            "uuid" => Bson::String(uuid::Uuid::new_v4().simple().to_string()),
            "print" => {
                let args = Args::new(input)?;
                let json = args.flag("json")?;
                let line = args
                    .array("values")?
                    .iter()
                    .map(|value| match value {
                        Bson::String(text) if !json => text.clone(),
                        value => display(value),
                    })
                    .collect::<Vec<_>>()
                    .join(if json { "\n" } else { " " });
                (self.sink)(Event::Output(line));
                Bson::Null
            }
            "result" => {
                let args = Args::new(input)?;
                let index = usize::try_from(args.number("index")?).map_err(|e| e.to_string())?;
                let value = match args.0.get("cursor") {
                    Some(Bson::Document(spec)) => ResultValue::Cursor(Box::new(query(spec)?)),
                    _ => result_value(args.0.get("value").cloned().unwrap_or(Bson::Null)),
                };
                let span = self.statements.get(index).cloned().unwrap_or_default();
                (self.sink)(Event::Result {
                    statement: index,
                    span,
                    value,
                    elapsed: self.started.elapsed(),
                });
                Bson::Null
            }
            "open" => {
                let Bson::Document(spec) = input else {
                    return Err("open takes a cursor.".to_owned());
                };
                let cursor = self.host.open(&query(&spec)?)?;
                self.open_cursors.push(cursor);
                number(cursor)
            }
            "next" => {
                let cursor = as_u64(&input, "cursor")?;
                let batch = self.host.next_batch(cursor)?;
                if batch.is_empty() {
                    self.open_cursors.retain(|open| *open != cursor);
                }
                Bson::Array(batch.into_iter().map(Bson::Document).collect())
            }
            "close" => {
                let cursor = as_u64(&input, "cursor")?;
                self.open_cursors.retain(|open| *open != cursor);
                self.host.close(cursor);
                Bson::Null
            }
            "count" => {
                let args = Args::new(input)?;
                let options = Args(args.document_or_empty("options")?);
                let count = self.host.count(
                    &args.namespace()?,
                    args.document_or_empty("filter")?,
                    CountOptions {
                        skip: options.optional_number("skip")?.unwrap_or(0),
                        limit: options.optional_number("limit")?.unwrap_or(0),
                        max_time: options.max_time()?,
                    },
                )?;
                number(count)
            }
            "estimatedCount" => number(self.host.estimated_count(&Args::new(input)?.namespace()?)?),
            "distinct" => {
                let args = Args::new(input)?;
                let values = self.host.distinct(
                    &args.namespace()?,
                    args.string("key")?,
                    args.document_or_empty("filter")?,
                )?;
                Bson::Array(values)
            }
            "write" => self.write(Args::new(input)?)?,
            "bulkWrite" => self.bulk_write(Args::new(input)?)?,
            "listDatabases" => Bson::Array(
                self.host
                    .list_databases()?
                    .into_iter()
                    .map(Bson::Document)
                    .collect(),
            ),
            "listCollections" => {
                let Bson::String(database) = input else {
                    return Err("listCollections takes a database name.".to_owned());
                };
                Bson::Array(
                    self.host
                        .list_collections(&database)?
                        .into_iter()
                        .map(Bson::String)
                        .collect(),
                )
            }
            other => return Err(format!("Unknown shell call {other}.")),
        })
    }

    fn write(&mut self, args: Args) -> Result<Bson, String> {
        let namespace = args.namespace()?;
        let op = args.string("op")?.to_owned();
        let write = parse_write(&op, &args)?;
        self.guard(&op, &namespace, &write)?;
        let outcome = self.host.write(&namespace, write)?;
        Ok(write_result(&op, outcome))
    }

    fn bulk_write(&mut self, args: Args) -> Result<Bson, String> {
        let namespace = args.namespace()?;
        let operations = args
            .array("operations")?
            .iter()
            .map(|operation| match operation {
                Bson::Document(operation) if operation.len() == 1 => {
                    let (op, body) = operation.iter().next().expect("one key");
                    let Bson::Document(body) = body else {
                        return Err(format!("bulkWrite: {op} takes a document."));
                    };
                    let body = Args(body.clone());
                    let body = match op.as_str() {
                        "replaceOne" => Args(rename(body.0, "replacement", "update")),
                        _ => body,
                    };
                    let body = match op.as_str() {
                        "insertOne" => Args(doc! {
                            "documents": [body.0.get("document").cloned().unwrap_or(Bson::Null)],
                        }),
                        _ => Args(options_inline(body.0)),
                    };
                    Ok((op.clone(), parse_write(op, &body)?))
                }
                _ => Err("bulkWrite operations look like { insertOne: { document } }.".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if operations.is_empty() {
            return Err("bulkWrite needs at least one operation.".to_owned());
        }
        if self.host.read_only() {
            return Err(read_only());
        }
        if !self.confirmed {
            let documents = operations.len() as u64;
            self.ask("bulkWrite", &namespace, documents)?;
        }
        let mut totals = WriteOutcome::default();
        let mut inserted = Document::new();
        let mut upserted = Document::new();
        for (index, (_, write)) in operations.into_iter().enumerate() {
            let outcome = self.host.write(&namespace, write)?;
            for id in outcome.inserted_ids {
                inserted.insert(index.to_string(), id);
            }
            if let Some(id) = outcome.upserted_id {
                upserted.insert(index.to_string(), id);
            }
            totals.matched += outcome.matched;
            totals.modified += outcome.modified;
            totals.deleted += outcome.deleted;
        }
        Ok(Bson::Document(doc! {
            "acknowledged": true,
            "insertedCount": number(inserted.len() as u64),
            "insertedIds": inserted,
            "matchedCount": number(totals.matched),
            "modifiedCount": number(totals.modified),
            "deletedCount": number(totals.deleted),
            "upsertedCount": number(upserted.len() as u64),
            "upsertedIds": upserted,
        }))
    }

    /// Checks a write may go ahead, asking the user before the run's first.
    fn guard(&mut self, op: &str, namespace: &Namespace, write: &Write) -> Result<(), String> {
        if self.host.read_only() {
            return Err(read_only());
        }
        if self.confirmed {
            return Ok(());
        }
        let matching = |host: &mut Box<dyn Host>, filter: &Document, many: bool| {
            let options = CountOptions {
                limit: if many { 0 } else { 1 },
                ..CountOptions::default()
            };
            host.count(namespace, filter.clone(), options)
        };
        let documents = match write {
            Write::Insert(documents) => documents.len() as u64,
            Write::Update { filter, many, .. } | Write::Delete { filter, many, .. } => {
                matching(&mut self.host, filter, *many)?
            }
            Write::Replace { filter, .. }
            | Write::FindOneAndUpdate { filter, .. }
            | Write::FindOneAndReplace { filter, .. }
            | Write::FindOneAndDelete { filter, .. } => matching(&mut self.host, filter, false)?,
        };
        self.ask(op, namespace, documents)
    }

    fn ask(&mut self, op: &str, namespace: &Namespace, documents: u64) -> Result<(), String> {
        let summary = WriteSummary {
            operation: op.to_owned(),
            namespace: namespace.clone(),
            documents,
        };
        if self.host.confirm_write(&summary) {
            self.confirmed = true;
            Ok(())
        } else {
            self.declined = true;
            Err("Stopped: the write was not confirmed.".to_owned())
        }
    }
}

fn read_only() -> String {
    "This connection is read-only.".to_owned()
}

/// Moves `from` to `to` in a bulkWrite body.
fn rename(mut body: Document, from: &str, to: &str) -> Document {
    if let Some(value) = body.remove(from) {
        body.insert(to, value);
    }
    body
}

/// A bulkWrite body carries its options next to filter and update.
fn options_inline(mut body: Document) -> Document {
    let mut options = Document::new();
    for key in ["upsert", "arrayFilters", "collation", "hint"] {
        if let Some(value) = body.remove(key) {
            options.insert(key, value);
        }
    }
    body.insert("options", options);
    body
}

fn parse_write(op: &str, args: &Args) -> Result<Write, String> {
    let options = Args(args.document_or_empty("options")?);
    let filter = || args.required_document("filter", op);
    let update = || match args.0.get("update") {
        Some(update @ (Bson::Document(_) | Bson::Array(_))) => Ok(update.clone()),
        _ => Err(format!(
            "{op} needs an update document, e.g. {{ $set: {{ a: 1 }} }}."
        )),
    };
    let replacement = || args.required_document("update", op);
    let update_options = || -> Result<UpdateOptions, String> {
        Ok(UpdateOptions {
            upsert: options.flag("upsert")?,
            array_filters: options.optional_documents("arrayFilters")?,
            collation: options.optional_document("collation")?,
            hint: options.0.get("hint").cloned(),
        })
    };
    let modify_options = || -> Result<FindAndModifyOptions, String> {
        let after = match options.0.get("returnDocument") {
            Some(Bson::String(when)) => when == "after",
            _ => options.flag("returnNewDocument")?,
        };
        Ok(FindAndModifyOptions {
            sort: options.optional_document("sort")?,
            projection: options.optional_document("projection")?,
            upsert: options.flag("upsert")?,
            return_after: after,
            array_filters: options.optional_documents("arrayFilters")?,
            collation: options.optional_document("collation")?,
            hint: options.0.get("hint").cloned(),
        })
    };
    let delete_options = || -> Result<DeleteOptions, String> {
        Ok(DeleteOptions {
            collation: options.optional_document("collation")?,
            hint: options.0.get("hint").cloned(),
        })
    };
    Ok(match op {
        "insertOne" | "insertMany" => {
            let documents = args
                .array("documents")?
                .iter()
                .map(|document| match document {
                    Bson::Document(document) => Ok(document.clone()),
                    _ => Err(format!("{op} takes documents in braces, e.g. {{ a: 1 }}.")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if documents.is_empty() {
                return Err(format!("{op} needs at least one document."));
            }
            Write::Insert(documents)
        }
        "updateOne" | "updateMany" => Write::Update {
            filter: filter()?,
            update: update()?,
            many: op == "updateMany",
            options: update_options()?,
        },
        "replaceOne" => Write::Replace {
            filter: filter()?,
            replacement: replacement()?,
            options: update_options()?,
        },
        "deleteOne" | "deleteMany" => Write::Delete {
            filter: filter()?,
            many: op == "deleteMany",
            options: delete_options()?,
        },
        "findOneAndUpdate" => Write::FindOneAndUpdate {
            filter: filter()?,
            update: update()?,
            options: modify_options()?,
        },
        "findOneAndReplace" => Write::FindOneAndReplace {
            filter: filter()?,
            replacement: replacement()?,
            options: modify_options()?,
        },
        "findOneAndDelete" => Write::FindOneAndDelete {
            filter: filter()?,
            options: modify_options()?,
        },
        other => return Err(format!("{other} is not a write the shell knows.")),
    })
}

/// The value a write returns to the script, shaped like mongosh's.
fn write_result(op: &str, outcome: WriteOutcome) -> Bson {
    let upserted = outcome.upserted_id.clone();
    match op {
        "insertOne" => Bson::Document(doc! {
            "acknowledged": true,
            "insertedId": outcome.inserted_ids.into_iter().next().unwrap_or(Bson::Null),
        }),
        "insertMany" => {
            let ids: Document = outcome
                .inserted_ids
                .into_iter()
                .enumerate()
                .map(|(index, id)| (index.to_string(), id))
                .collect();
            Bson::Document(doc! { "acknowledged": true, "insertedIds": ids })
        }
        "deleteOne" | "deleteMany" => Bson::Document(doc! {
            "acknowledged": true,
            "deletedCount": number(outcome.deleted),
        }),
        "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete" => {
            outcome.document.map_or(Bson::Null, Bson::Document)
        }
        _ => Bson::Document(doc! {
            "acknowledged": true,
            "insertedId": upserted.clone().unwrap_or(Bson::Null),
            "matchedCount": number(outcome.matched),
            "modifiedCount": number(outcome.modified),
            "upsertedCount": number(u64::from(upserted.is_some())),
        }),
    }
}

/// Like mongosh: on one line when short, else indented.
fn display(value: &Bson) -> String {
    let compact = format_value(value, Style::Compact);
    if compact.chars().count() <= 80 {
        compact
    } else {
        format_value(value, Style::Pretty(2))
    }
}

/// A count for the script: a plain JavaScript number.
fn number(n: u64) -> Bson {
    i32::try_from(n).map_or(Bson::Double(n as f64), Bson::Int32)
}

fn as_u64(value: &Bson, name: &str) -> Result<u64, String> {
    let n = match value {
        Bson::Int32(n) => f64::from(*n),
        Bson::Int64(n) => *n as f64,
        Bson::Double(n) => *n,
        _ => return Err(format!("{name} must be a number.")),
    };
    if n.is_finite() {
        Ok(n.abs().trunc() as u64)
    } else {
        Err(format!("{name} must be a number."))
    }
}

/// An array of documents is shown as documents; anything else as a value.
fn result_value(value: Bson) -> ResultValue {
    match value {
        Bson::Array(items)
            if !items.is_empty() && items.iter().all(|item| matches!(item, Bson::Document(_))) =>
        {
            ResultValue::Documents(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        Bson::Document(document) => Some(document),
                        _ => None,
                    })
                    .collect(),
            )
        }
        Bson::Document(document) if document.is_empty() => {
            ResultValue::Value(Bson::Document(document))
        }
        value => ResultValue::Value(value),
    }
}

/// A cursor's spec from the prelude: `{ kind, ns, filter | pipeline, options }`.
fn query(spec: &Document) -> Result<Query, String> {
    let args = Args(spec.clone());
    let namespace = args.namespace()?;
    let options = Args(args.document_or_empty("options")?);
    let kind = match args.string("kind")? {
        "find" => QueryKind::Find {
            filter: args.document_or_empty("filter")?,
            options: Box::new(FindOptions {
                projection: options.optional_document("projection")?,
                sort: options.optional_document("sort")?,
                skip: options.optional_number("skip")?.unwrap_or(0),
                limit: options.optional_number("limit")?.unwrap_or(0),
                hint: options.0.get("hint").cloned(),
                collation: options.optional_document("collation")?,
                max_time: options.max_time()?,
            }),
        },
        _ => QueryKind::Aggregate {
            pipeline: args
                .array("pipeline")?
                .iter()
                .map(|stage| match stage {
                    Bson::Document(stage) => Ok(stage.clone()),
                    _ => Err(
                        "Each pipeline stage must be a document, e.g. { $match: {} }.".to_owned(),
                    ),
                })
                .collect::<Result<_, _>>()?,
            options: AggregateOptions {
                allow_disk_use: options.0.get_bool("allowDiskUse").ok(),
                hint: options.0.get("hint").cloned(),
                collation: options.optional_document("collation")?,
                max_time: options.max_time()?,
            },
        },
    };
    Ok(Query { namespace, kind })
}

/// Reads the fields of a call's payload, with errors for the user.
struct Args(Document);

impl Args {
    fn new(input: Bson) -> Result<Self, String> {
        match input {
            Bson::Document(document) => Ok(Self(document)),
            _ => Err("Expected a document.".to_owned()),
        }
    }

    fn namespace(&self) -> Result<Namespace, String> {
        let ns = self
            .0
            .get_document("ns")
            .map_err(|_| "Missing collection.".to_owned())?;
        let part = |key: &str| {
            ns.get_str(key)
                .map(str::to_owned)
                .map_err(|e| e.to_string())
        };
        Ok(Namespace {
            database: part("db")?,
            collection: part("coll")?,
        })
    }

    fn string(&self, key: &str) -> Result<&str, String> {
        self.0
            .get_str(key)
            .map_err(|_| format!("{key} must be a string."))
    }

    fn flag(&self, key: &str) -> Result<bool, String> {
        match self.0.get(key) {
            None | Some(Bson::Null) => Ok(false),
            Some(Bson::Boolean(flag)) => Ok(*flag),
            Some(_) => Err(format!("{key} must be true or false.")),
        }
    }

    fn number(&self, key: &str) -> Result<u64, String> {
        as_u64(self.0.get(key).unwrap_or(&Bson::Null), key)
    }

    fn optional_number(&self, key: &str) -> Result<Option<u64>, String> {
        match self.0.get(key) {
            None | Some(Bson::Null) => Ok(None),
            Some(value) => as_u64(value, key).map(Some),
        }
    }

    fn max_time(&self) -> Result<Option<Duration>, String> {
        Ok(self
            .optional_number("maxTimeMS")?
            .map(Duration::from_millis))
    }

    fn array(&self, key: &str) -> Result<&Vec<Bson>, String> {
        self.0
            .get_array(key)
            .map_err(|_| format!("{key} must be an array."))
    }

    fn document_or_empty(&self, key: &str) -> Result<Document, String> {
        Ok(self.optional_document(key)?.unwrap_or_default())
    }

    fn optional_document(&self, key: &str) -> Result<Option<Document>, String> {
        match self.0.get(key) {
            None | Some(Bson::Null) => Ok(None),
            Some(Bson::Document(document)) => Ok(Some(document.clone())),
            Some(_) => Err(format!("{key} must be a document, e.g. {{ a: 1 }}.")),
        }
    }

    fn required_document(&self, key: &str, op: &str) -> Result<Document, String> {
        self.optional_document(key)?
            .ok_or_else(|| format!("{op} needs a {key} document, e.g. {{ _id: 1 }}."))
    }

    fn optional_documents(&self, key: &str) -> Result<Option<Vec<Document>>, String> {
        match self.0.get(key) {
            None | Some(Bson::Null) => Ok(None),
            Some(Bson::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Bson::Document(document) => Ok(document.clone()),
                    _ => Err(format!("{key} must hold documents.")),
                })
                .collect::<Result<_, _>>()
                .map(Some),
            Some(_) => Err(format!("{key} must be an array of documents.")),
        }
    }
}
