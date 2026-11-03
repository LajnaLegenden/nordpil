//! Runs scripts against an in-memory host that records what it was asked.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, doc};
use proptest::prelude::*;

use super::*;

/// What the fake host was asked and how it answers, shared with the test.
#[derive(Default)]
struct Shared {
    collections: HashMap<String, Vec<Document>>,
    queries: Vec<Query>,
    writes: Vec<(Namespace, Write)>,
    confirmations: Vec<WriteSummary>,
    /// The answer to write confirmations.
    allow_writes: bool,
    read_only: bool,
    /// Cancelled after this many checks.
    cancel_after: Option<usize>,
    checks: usize,
}

struct FakeHost {
    shared: Rc<RefCell<Shared>>,
    cursors: HashMap<CursorId, Vec<Document>>,
    next_id: CursorId,
}

/// Documents per batch, small so tests read several.
const BATCH: usize = 2;

impl Host for FakeHost {
    fn cancelled(&self) -> bool {
        let mut shared = self.shared.borrow_mut();
        shared.checks += 1;
        shared
            .cancel_after
            .is_some_and(|after| shared.checks > after)
    }

    fn read_only(&self) -> bool {
        self.shared.borrow().read_only
    }

    fn open(&mut self, query: &Query) -> Result<CursorId, String> {
        let mut shared = self.shared.borrow_mut();
        shared.queries.push(query.clone());
        let mut documents = shared
            .collections
            .get(&query.namespace.collection)
            .cloned()
            .unwrap_or_default();
        if let QueryKind::Find { options, .. } = &query.kind
            && options.limit > 0
        {
            documents.truncate(options.limit as usize);
        }
        self.next_id += 1;
        self.cursors.insert(self.next_id, documents);
        Ok(self.next_id)
    }

    fn next_batch(&mut self, cursor: CursorId) -> Result<Vec<Document>, String> {
        let documents = self.cursors.get_mut(&cursor).ok_or("No such cursor.")?;
        let take = documents.len().min(BATCH);
        Ok(documents.drain(..take).collect())
    }

    fn close(&mut self, cursor: CursorId) {
        self.cursors.remove(&cursor);
    }

    fn count(
        &mut self,
        namespace: &Namespace,
        _filter: Document,
        options: CountOptions,
    ) -> Result<u64, String> {
        let shared = self.shared.borrow();
        let count = shared
            .collections
            .get(&namespace.collection)
            .map_or(0, Vec::len) as u64;
        Ok(match options.limit {
            0 => count,
            limit => count.min(limit),
        })
    }

    fn estimated_count(&mut self, namespace: &Namespace) -> Result<u64, String> {
        self.count(namespace, Document::new(), CountOptions::default())
    }

    fn distinct(
        &mut self,
        namespace: &Namespace,
        key: &str,
        _filter: Document,
    ) -> Result<Vec<Bson>, String> {
        let shared = self.shared.borrow();
        let mut values: Vec<Bson> = Vec::new();
        for document in shared
            .collections
            .get(&namespace.collection)
            .into_iter()
            .flatten()
        {
            if let Some(value) = document.get(key)
                && !values.contains(value)
            {
                values.push(value.clone());
            }
        }
        Ok(values)
    }

    fn write(&mut self, namespace: &Namespace, write: Write) -> Result<WriteOutcome, String> {
        let mut shared = self.shared.borrow_mut();
        shared.writes.push((namespace.clone(), write.clone()));
        Ok(match write {
            Write::Insert(documents) => WriteOutcome {
                inserted_ids: documents
                    .iter()
                    .map(|document| document.get("_id").cloned().unwrap_or(Bson::Int32(1)))
                    .collect(),
                ..WriteOutcome::default()
            },
            Write::Delete { .. } => WriteOutcome {
                deleted: 2,
                ..WriteOutcome::default()
            },
            _ => WriteOutcome {
                matched: 1,
                modified: 1,
                ..WriteOutcome::default()
            },
        })
    }

    fn list_databases(&mut self) -> Result<Vec<Document>, String> {
        Ok(vec![doc! { "name": "shop", "empty": false }])
    }

    fn list_collections(&mut self, _database: &str) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = self.shared.borrow().collections.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    fn confirm_write(&mut self, summary: &WriteSummary) -> bool {
        let mut shared = self.shared.borrow_mut();
        shared.confirmations.push(summary.clone());
        shared.allow_writes
    }
}

struct Run {
    outcome: Outcome,
    events: Vec<Event>,
    shared: Rc<RefCell<Shared>>,
}

impl Run {
    fn results(&self) -> Vec<(usize, ResultValue)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Result {
                    statement, value, ..
                } => Some((*statement, value.clone())),
                Event::Output(_) => None,
            })
            .collect()
    }

    fn output(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Output(line) => Some(line.clone()),
                Event::Result { .. } => None,
            })
            .collect()
    }

    fn error(&self) -> ScriptError {
        match &self.outcome {
            Outcome::Failed(error) => error.clone(),
            other => panic!("expected an error, got {other:?}"),
        }
    }
}

fn users() -> Vec<Document> {
    (1..=3)
        .map(|n| doc! { "_id": n, "name": format!("user {n}") })
        .collect()
}

fn run_with(script: &str, setup: impl FnOnce(&mut Shared)) -> Run {
    let mut shared = Shared {
        allow_writes: true,
        ..Shared::default()
    };
    shared.collections.insert("users".to_owned(), users());
    setup(&mut shared);
    let shared = Rc::new(RefCell::new(shared));
    let host = FakeHost {
        shared: Rc::clone(&shared),
        cursors: HashMap::new(),
        next_id: 0,
    };
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&events);
    let outcome = run(script, "shop", Box::new(host), move |event| {
        sink.borrow_mut().push(event);
    });
    let events = events.borrow().clone();
    Run {
        outcome,
        events,
        shared,
    }
}

fn run_script(script: &str) -> Run {
    run_with(script, |_| {})
}

fn namespace(collection: &str) -> Namespace {
    Namespace {
        database: "shop".to_owned(),
        collection: collection.to_owned(),
    }
}

#[test]
fn reports_one_result_per_expression_statement() {
    let run = run_script("const n = 2\nn * 21\nlet m = 1\n'done'");
    assert_eq!(run.outcome, Outcome::Finished);
    assert_eq!(
        run.results(),
        [
            (1, ResultValue::Value(Bson::Int32(42))),
            (3, ResultValue::Value(Bson::String("done".to_owned()))),
        ]
    );
}

#[test]
fn an_unread_cursor_is_its_query() {
    let run =
        run_script("db.users.find({ age: { $gt: 30 } }, { name: 1 }).sort({ name: -1 }).limit(5)");
    assert_eq!(
        run.results(),
        [(
            0,
            ResultValue::Cursor(Box::new(Query {
                namespace: namespace("users"),
                kind: QueryKind::Find {
                    filter: doc! { "age": { "$gt": 30 } },
                    options: Box::new(FindOptions {
                        projection: Some(doc! { "name": 1 }),
                        sort: Some(doc! { "name": -1 }),
                        limit: 5,
                        ..FindOptions::default()
                    }),
                },
            }))
        )]
    );
    assert!(run.shared.borrow().queries.is_empty());
}

#[test]
fn reads_cursors_in_batches() {
    let run = run_script(
        "db.users.find().toArray()\n\
         db.getCollection('users').aggregate([{ $match: {} }]).itcount()\n\
         let names = []\n\
         db.users.find().forEach(user => names.push(user.name))\n\
         names.length\n\
         db.users.findOne()",
    );
    assert_eq!(run.outcome, Outcome::Finished);
    let results = run.results();
    assert_eq!(results[0], (0, ResultValue::Documents(users())));
    assert_eq!(results[1], (1, ResultValue::Value(Bson::Int32(3))));
    assert_eq!(results[2], (4, ResultValue::Value(Bson::Int32(3))));
    assert_eq!(
        results[3],
        (5, ResultValue::Value(Bson::Document(users()[0].clone())))
    );
}

#[test]
fn reads_cursors_with_array_methods() {
    let run = run_script(
        "db.users.find().filter(user => user._id > 1).length\n\
         db.users.find().reduce((sum, user) => sum + user._id, 0)\n\
         db.users.find().find(user => user._id === 2).name\n\
         db.users.find().some(user => user._id === 9)\n\
         db.users.find().every(user => 'name' in user)\n\
         db.users.find().flatMap(user => [user._id, user._id]).length",
    );
    assert_eq!(run.outcome, Outcome::Finished);
    let values: Vec<_> = run.results().into_iter().map(|(_, value)| value).collect();
    assert_eq!(
        values,
        [
            ResultValue::Value(Bson::Int32(2)),
            ResultValue::Value(Bson::Int32(6)),
            ResultValue::Value(Bson::String("user 2".to_owned())),
            ResultValue::Value(Bson::Boolean(false)),
            ResultValue::Value(Bson::Boolean(true)),
            ResultValue::Value(Bson::Int32(6)),
        ]
    );
}

#[test]
fn prints_like_mongosh() {
    let run = run_script("print('a', 1, { b: 'c' })\nprintjson('s')\nconsole.log([1, 2])");
    assert_eq!(run.output(), ["a 1 { b: 'c' }", "'s'", "[ 1, 2 ]"]);
}

#[test]
fn keeps_bson_types_and_mongosh_numbers() {
    let run = run_script(
        "db.users.insertOne({\n\
           _id: ObjectId('507f1f77bcf86cd799439011'),\n\
           small: 1, big: 3000000000, half: 1.5, long: NumberLong('5'),\n\
           when: ISODate('2024-01-02T03:04:05Z'), price: NumberDecimal('1.10'),\n\
           re: /a\\/b/i, id: UUID('0e9a3a4e-3c7e-4c9b-9e0e-2f1f2d6c9a11'),\n\
           ts: Timestamp({ t: 5, i: 1 }), low: MinKey(), d: Double(2)\n\
         })",
    );
    assert_eq!(run.outcome, Outcome::Finished);
    let writes = &run.shared.borrow().writes;
    let Write::Insert(documents) = &writes[0].1 else {
        panic!("not an insert");
    };
    let id = Binary {
        subtype: BinarySubtype::Uuid,
        bytes: vec![
            0x0e, 0x9a, 0x3a, 0x4e, 0x3c, 0x7e, 0x4c, 0x9b, 0x9e, 0x0e, 0x2f, 0x1f, 0x2d, 0x6c,
            0x9a, 0x11,
        ],
    };
    assert_eq!(
        documents[0],
        doc! {
            "_id": ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap(),
            "small": 1, "big": 3e9, "half": 1.5, "long": 5_i64,
            "when": DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").unwrap(),
            "price": "1.10".parse::<Decimal128>().unwrap(),
            "re": Regex { pattern: "a\\/b".to_owned().try_into().unwrap(), options: "i".to_owned().try_into().unwrap() },
            "id": id,
            "ts": Timestamp { time: 5, increment: 1 },
            "low": Bson::MinKey,
            "d": 2.0,
        }
    );
    assert_eq!(
        run.results(),
        [(
            0,
            ResultValue::Value(Bson::Document(doc! {
                "acknowledged": true,
                "insertedId": ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap(),
            }))
        )]
    );
}

#[test]
fn asks_once_before_the_first_write() {
    let run = run_script(
        "db.users.deleteMany({ old: true })\ndb.users.updateOne({ _id: 1 }, { $set: { a: 1 } })",
    );
    assert_eq!(run.outcome, Outcome::Finished);
    let shared = run.shared.borrow();
    assert_eq!(
        shared.confirmations,
        [WriteSummary {
            operation: "deleteMany".to_owned(),
            namespace: namespace("users"),
            documents: 3,
        }]
    );
    assert_eq!(shared.writes.len(), 2);
    assert_eq!(
        run.results()[1].1,
        ResultValue::Value(Bson::Document(doc! {
            "acknowledged": true, "insertedId": Bson::Null,
            "matchedCount": 1, "modifiedCount": 1, "upsertedCount": 0,
        }))
    );
}

#[test]
fn a_declined_write_stops_the_run_even_inside_try() {
    let run = run_with(
        "try { db.users.deleteOne({ _id: 1 }) } catch (e) {}\nprint('after')",
        |shared| shared.allow_writes = false,
    );
    assert_eq!(run.outcome, Outcome::Declined);
    assert!(run.shared.borrow().writes.is_empty());
    assert!(run.output().is_empty());
}

#[test]
fn read_only_connections_refuse_writes_without_asking() {
    let run = run_with("db.users.insertOne({ a: 1 })", |shared| {
        shared.read_only = true
    });
    assert_eq!(run.error().message, "This connection is read-only.");
    assert!(run.shared.borrow().confirmations.is_empty());
}

#[test]
fn errors_point_at_the_script() {
    let run = run_script("const a = 1\nprint(a)\n  missing.call()");
    let error = run.error();
    assert_eq!(error.message, "ReferenceError: missing is not defined");
    assert_eq!(error.statement, Some(2));
    assert_eq!((error.line, error.column), (Some(3), Some(3)));
    assert_eq!(run.output(), ["1"]);

    let run = run_script("db.users.find({ a: 1 )");
    assert!(
        run.error().message.starts_with("SyntaxError"),
        "{:?}",
        run.error()
    );
    assert_eq!(run.error().line, Some(1));
}

#[test]
fn use_and_show_work_like_mongosh() {
    let run = run_script("use other\ndb.users.find()\nshow collections\nshow dbs");
    assert_eq!(run.output(), ["switched to db other"]);
    let results = run.results();
    let ResultValue::Cursor(query) = &results[0].1 else {
        panic!("not a cursor");
    };
    assert_eq!(query.namespace.database, "other");
    assert_eq!(
        results[1],
        (
            2,
            ResultValue::Value(Bson::Array(vec![Bson::String("users".to_owned())]))
        )
    );
    assert_eq!(
        results[2],
        (
            3,
            ResultValue::Documents(vec![doc! { "name": "shop", "empty": false }])
        )
    );
}

#[test]
fn stop_interrupts_a_busy_loop() {
    let run = run_with("while (true) {}", |shared| shared.cancel_after = Some(50));
    assert_eq!(run.outcome, Outcome::Cancelled);
}

#[test]
fn the_heap_is_capped() {
    let run = run_script("const all = []\nwhile (true) all.push('x'.repeat(100000))");
    assert!(
        run.error().message.contains("more than 256 MB"),
        "{:?}",
        run.error()
    );
}

#[test]
fn reports_bulk_write_totals() {
    let run = run_script(
        "db.users.bulkWrite([\n\
           { insertOne: { document: { _id: 9 } } },\n\
           { updateOne: { filter: { _id: 1 }, update: { $set: { a: 1 } }, upsert: true } },\n\
           { deleteMany: { filter: {} } },\n\
         ])",
    );
    assert_eq!(run.outcome, Outcome::Finished);
    let shared = run.shared.borrow();
    assert_eq!(shared.confirmations[0].documents, 3);
    let Write::Update { options, .. } = &shared.writes[1].1 else {
        panic!("not an update");
    };
    assert!(options.upsert);
    let ResultValue::Value(Bson::Document(totals)) = &run.results()[0].1 else {
        panic!("no totals");
    };
    assert_eq!(totals.get("insertedCount"), Some(&Bson::Int32(1)));
    assert_eq!(totals.get("deletedCount"), Some(&Bson::Int32(2)));
}

/// BSON values that survive the trip to JavaScript and back unchanged.
/// Whole doubles come back as Int32, like in mongosh, so they're left out.
fn leaf() -> impl Strategy<Value = Bson> {
    prop_oneof![
        any::<i32>().prop_map(Bson::Int32),
        any::<i64>().prop_map(Bson::Int64),
        any::<f64>()
            .prop_filter("whole doubles turn into Int32", |n| n.fract() != 0.0)
            .prop_map(Bson::Double),
        "\\PC{0,8}".prop_map(Bson::String),
        any::<bool>().prop_map(Bson::Boolean),
        Just(Bson::Null),
        Just(Bson::MinKey),
        Just(Bson::MaxKey),
        any::<[u8; 12]>().prop_map(|bytes| Bson::ObjectId(ObjectId::from_bytes(bytes))),
        (-8_000_000_000_000_000_i64..8_000_000_000_000_000)
            .prop_map(|ms| Bson::DateTime(DateTime::from_millis(ms))),
        (any::<u32>(), any::<u32>())
            .prop_map(|(time, increment)| Bson::Timestamp(Timestamp { time, increment })),
        (any::<u8>(), proptest::collection::vec(any::<u8>(), 0..20)).prop_map(
            |(subtype, bytes)| Bson::Binary(Binary {
                subtype: BinarySubtype::from(subtype % 0x08),
                bytes,
            })
        ),
        ("[a-z.*+]{1,6}", "[imsux]{0,2}").prop_map(|(pattern, options)| {
            let mut options: Vec<char> = options.chars().collect();
            options.sort_unstable();
            options.dedup();
            Bson::RegularExpression(Regex {
                pattern: pattern.try_into().unwrap(),
                options: options.into_iter().collect::<String>().try_into().unwrap(),
            })
        }),
        (any::<i64>(), 0..6_u32).prop_map(|(n, scale)| {
            let text = format!("{n}E-{scale}");
            Bson::Decimal128(text.parse().unwrap())
        }),
    ]
}

fn document() -> impl Strategy<Value = Document> {
    let value = leaf().prop_recursive(2, 12, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Bson::Array),
            proptest::collection::vec(("[a-z]{1,4}", inner), 0..4)
                .prop_map(|fields| Bson::Document(fields.into_iter().collect())),
        ]
    });
    proptest::collection::vec(("[a-z]{1,4}", value), 0..6)
        .prop_map(|fields| fields.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A document read by a script and handed back keeps its BSON types.
    #[test]
    fn documents_survive_the_trip_through_javascript(document in document()) {
        let expected = document.clone();
        let run = run_with("db.things.findOne()", |shared| {
            shared.collections.insert("things".to_owned(), vec![document]);
        });
        prop_assert_eq!(&run.outcome, &Outcome::Finished);
        prop_assert_eq!(
            run.results(),
            vec![(0, ResultValue::Value(Bson::Document(expected)))]
        );
    }
}

#[test]
fn cursors_keep_the_time_limit_the_script_sets() {
    let run = run_script("db.users.find().maxTimeMS(1500)");
    let ResultValue::Cursor(query) = &run.results()[0].1 else {
        panic!("not a cursor");
    };
    let QueryKind::Find { options, .. } = &query.kind else {
        panic!("not a find");
    };
    assert_eq!(options.max_time, Some(Duration::from_millis(1500)));
}
