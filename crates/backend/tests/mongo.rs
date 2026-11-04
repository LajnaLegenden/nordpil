//! Runs against a real mongod in Docker. Run with `just test-int`.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt as _;
use mongodb::bson::{Document, doc};
use nordpil_backend::secrets::MemorySecrets;
use nordpil_backend::store::{ConnectionStore, SavedConnection};
use nordpil_backend::{
    AggregatePage, Backend, CancellationToken, Command, ErrorKind, FindQuery, Handler, Namespace,
    PasswordChange, PipelineQuery, Response,
};
use testcontainers_modules::mongo::Mongo;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use uuid::Uuid;

async fn send(backend: &Backend, command: Command) -> Response {
    backend.handle(command, CancellationToken::new()).await
}

/// Starts mongod and returns a backend connected to it.
async fn connected_backend(read_only: bool) -> (ContainerAsync<Mongo>, Backend, Uuid, String) {
    let mongo = Mongo::default().with_tag("8.0").start().await.unwrap();
    let port = mongo.get_host_port_ipv4(27017).await.unwrap();
    let dir = std::env::temp_dir().join(format!("nordpil-it-{}", Uuid::new_v4()));
    let backend = Backend::new(
        ConnectionStore::new(dir.join("connections.toml")),
        MemorySecrets::default(),
    );

    let id = Uuid::new_v4();
    let uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true");
    let connection = SavedConnection {
        id,
        name: "test".to_owned(),
        uri: uri.clone(),
        color: None,
        read_only,
    };
    let save = Command::SaveConnection {
        connection,
        password: PasswordChange::Keep,
    };
    assert!(matches!(
        send(&backend, save).await,
        Response::ConnectionSaved(Ok(_))
    ));

    let connect = Command::Connect {
        id,
        password: None,
        remember_password: false,
    };
    let Response::Connected { result, .. } = send(&backend, connect).await else {
        panic!("unexpected response");
    };
    let databases = result.unwrap();
    assert!(databases.iter().any(|db| db.name == "admin"));
    (mongo, backend, id, uri)
}

async fn collection_names(backend: &Backend, connection: Uuid, database: &str) -> Vec<String> {
    let list = Command::ListCollections {
        connection,
        database: database.to_owned(),
    };
    let Response::Collections { result, .. } = send(backend, list).await else {
        panic!("unexpected response");
    };
    result.unwrap().into_iter().map(|c| c.name).collect()
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn creates_lists_and_drops_collections() {
    let (_mongo, backend, id, _) = connected_backend(false).await;

    let create = Command::CreateCollection {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    let Response::CollectionCreated { result, .. } = send(&backend, create).await else {
        panic!("unexpected response");
    };
    result.unwrap();
    assert_eq!(collection_names(&backend, id, "shop").await, ["orders"]);

    let drop = Command::DropCollection {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    let Response::CollectionDropped { result, .. } = send(&backend, drop).await else {
        panic!("unexpected response");
    };
    result.unwrap();
    assert!(collection_names(&backend, id, "shop").await.is_empty());
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn read_only_connection_rejects_writes() {
    let (_mongo, backend, id, _) = connected_backend(true).await;

    let create = Command::CreateCollection {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    let Response::CollectionCreated { result, .. } = send(&backend, create).await else {
        panic!("unexpected response");
    };

    assert_eq!(result.unwrap_err().kind, ErrorKind::ReadOnly);
    assert!(collection_names(&backend, id, "shop").await.is_empty());

    let write = Command::WriteAggregate {
        namespace: Namespace {
            connection: id,
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
        },
        query: Box::new(pipeline(vec![doc! { "$out": "copy" }])),
    };
    let Response::AggregateWritten(result) = send(&backend, write).await else {
        panic!("unexpected response");
    };
    assert_eq!(result.unwrap_err().kind, ErrorKind::ReadOnly);
    assert!(collection_names(&backend, id, "shop").await.is_empty());
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn finds_pages_and_counts_documents() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
    let orders = client.database("shop").collection::<Document>("orders");
    orders
        .insert_many((0..30).map(|n| doc! { "n": n, "even": n % 2 == 0 }))
        .await
        .unwrap();
    let namespace = Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };

    let find = Command::Find {
        namespace: namespace.clone(),
        query: Box::new(FindQuery {
            filter: doc! { "even": true },
            projection: Some(doc! { "_id": 0, "n": 1 }),
            sort: Some(doc! { "n": -1 }),
            skip: 2,
            limit: 3,
            ..FindQuery::default()
        }),
    };
    let Response::Documents(result) = send(&backend, find).await else {
        panic!("unexpected response");
    };
    let documents = result.unwrap();
    assert_eq!(
        documents,
        [doc! { "n": 24 }, doc! { "n": 22 }, doc! { "n": 20 }]
    );

    for (filter, expected) in [(doc! {}, 30), (doc! { "even": true }, 15)] {
        let count = Command::Count {
            namespace: namespace.clone(),
            filter,
            hint: None,
        };
        let Response::Count(result) = send(&backend, count).await else {
            panic!("unexpected response");
        };
        assert_eq!(result.unwrap(), Some(expected));
    }

    let Response::Fields { result, .. } = send(&backend, Command::SampleFields(namespace)).await
    else {
        panic!("unexpected response");
    };
    let paths: Vec<_> = result
        .unwrap()
        .into_iter()
        .map(|field| field.path)
        .collect();
    assert_eq!(paths, ["_id", "n", "even"]);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn inserts_edits_and_deletes_documents() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let namespace = Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    let written = |response: Response| match response {
        Response::Written { result, .. } => result,
        _ => panic!("unexpected response"),
    };

    let insert = Command::InsertDocuments {
        namespace: namespace.clone(),
        documents: vec![
            doc! { "_id": 1, "status": "new", "address": { "city": "x", "zip": "1" } },
            doc! { "_id": 2, "status": "new" },
        ],
    };
    assert_eq!(written(send(&backend, insert).await), Ok(2));

    let original = doc! { "_id": 1, "status": "new", "address": { "city": "x", "zip": "1" } };
    let edit = |updated: Document| Command::UpdateDocument {
        namespace: namespace.clone(),
        original: Box::new(original.clone()),
        updated: Box::new(updated),
    };
    let updated = doc! { "_id": 1, "status": "paid", "address": { "city": "y", "zip": "1" } };
    assert_eq!(written(send(&backend, edit(updated.clone())).await), Ok(1));

    // The same edit again starts from a stale original, so it must conflict.
    let stale = written(send(&backend, edit(doc! { "_id": 1, "status": "x" })).await);
    assert_eq!(stale.unwrap_err().kind, ErrorKind::Conflict);

    let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
    let orders = client.database("shop").collection::<Document>("orders");
    assert_eq!(
        orders.find_one(doc! { "_id": 1 }).await.unwrap(),
        Some(updated)
    );

    let update_many = Command::UpdateMany {
        namespace: namespace.clone(),
        filter: doc! { "status": "new" },
        update: doc! { "$set": { "status": "seen" } }.into(),
    };
    assert_eq!(written(send(&backend, update_many).await), Ok(1));

    let delete = Command::DeleteDocument {
        namespace: namespace.clone(),
        id: 2.into(),
    };
    assert_eq!(written(send(&backend, delete).await), Ok(1));
    let delete_many = Command::DeleteMany {
        namespace,
        filter: doc! {},
    };
    assert_eq!(written(send(&backend, delete_many).await), Ok(1));
    assert_eq!(orders.count_documents(doc! {}).await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn creates_hides_lists_and_drops_indexes() {
    let (_mongo, backend, id, _) = connected_backend(false).await;
    let namespace = Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: "orders".to_owned(),
    };
    let changed = |response: Response| match response {
        Response::IndexChanged { result, .. } => result,
        _ => panic!("unexpected response"),
    };
    let list = || async {
        let Response::Indexes { result, .. } =
            send(&backend, Command::ListIndexes(namespace.clone())).await
        else {
            panic!("unexpected response");
        };
        result.unwrap()
    };

    let create = Command::CreateIndex {
        namespace: namespace.clone(),
        keys: doc! { "email": 1, "created": -1 },
        options: doc! { "unique": true },
    };
    changed(send(&backend, create).await).unwrap();
    let hide = Command::SetIndexHidden {
        namespace: namespace.clone(),
        name: "email_1_created_-1".to_owned(),
        hidden: true,
    };
    changed(send(&backend, hide).await).unwrap();

    let indexes = list().await;
    let names: Vec<_> = indexes.iter().map(|index| index.name.as_str()).collect();
    assert_eq!(names, ["_id_", "email_1_created_-1"]);
    let created = &indexes[1];
    assert_eq!(created.keys(), doc! { "email": 1, "created": -1 });
    assert!(created.spec.get_bool("unique").unwrap());
    assert!(created.spec.get_bool("hidden").unwrap());
    assert!(created.size.is_some());

    let drop = Command::DropIndex {
        namespace: namespace.clone(),
        name: "email_1_created_-1".to_owned(),
    };
    changed(send(&backend, drop).await).unwrap();
    assert_eq!(list().await.len(), 1);
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn exports_and_imports_every_format() {
    use std::sync::Arc;

    use nordpil_backend::transfer::{
        ExportJob, ExportQuery, Format, ImportJob, JsonMode, Progress,
    };

    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
    let source = client.database("shop").collection::<Document>("orders");
    let documents: Vec<Document> = (0..2500)
        .map(|n| {
            doc! {
                "_id": n,
                "name": format!("order {n}"),
                "total": f64::from(n) + 0.5,
                "big": 9_007_199_254_740_993_i64,
                "address": { "city": "Oslo", "zip": "0150" },
            }
        })
        .collect();
    source.insert_many(documents.clone()).await.unwrap();
    let dir = std::env::temp_dir().join(format!("nordpil-transfer-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let namespace = |collection: &str| Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: collection.to_owned(),
    };

    for (format, file) in [
        (Format::JsonArray, "orders.json"),
        (Format::Ndjson, "orders.ndjson"),
        (Format::Csv, "orders.csv"),
    ] {
        let path = dir.join(file);
        let export = ExportJob {
            namespace: namespace("orders"),
            query: ExportQuery::Find(Box::new(FindQuery {
                sort: Some(doc! { "_id": 1 }),
                ..FindQuery::default()
            })),
            path: path.clone(),
            format,
            json_mode: JsonMode::Canonical,
            fields: ["_id", "name", "total", "big", "address.city", "address.zip"]
                .map(str::to_owned)
                .into(),
            progress: Arc::new(Progress::default()),
        };
        let Response::Exported(result) = send(&backend, Command::Export(Box::new(export))).await
        else {
            panic!("unexpected response");
        };
        assert_eq!(result.unwrap(), 2500, "{file}");

        let target = format!("copy_{file}").replace('.', "_");
        let import = ImportJob {
            namespace: namespace(&target),
            path,
            format,
            csv_as_text: false,
            progress: Arc::new(Progress::default()),
        };
        let Response::Imported { result, .. } =
            send(&backend, Command::Import(Box::new(import))).await
        else {
            panic!("unexpected response");
        };
        let summary = result.unwrap();
        assert_eq!((summary.inserted, summary.failed), (2500, 0), "{file}");

        let copied: Vec<Document> = client
            .database("shop")
            .collection::<Document>(&target)
            .find(doc! {})
            .sort(doc! { "_id": 1 })
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        // CSV keeps "0150" as text and reads whole totals like "1.5" back as doubles.
        assert_eq!(copied, documents, "{file}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs Docker"]
async fn indexes_and_remembers_collection_names() {
    let (_mongo, backend, id, _) = connected_backend(false).await;
    for (database, collection) in [("shop", "orders"), ("shop", "users"), ("logs", "events")] {
        let create = Command::CreateCollection {
            connection: id,
            database: database.to_owned(),
            collection: collection.to_owned(),
        };
        assert!(matches!(
            send(&backend, create).await,
            Response::CollectionCreated { result: Ok(()), .. }
        ));
    }

    let index = Command::IndexCollections {
        connection: id,
        remember: true,
    };
    let Response::CollectionIndex { result, .. } = send(&backend, index).await else {
        panic!("unexpected response");
    };
    let names = |databases: &[nordpil_backend::navigation::KnownDatabase], name: &str| {
        databases
            .iter()
            .find(|known| known.database == name)
            .map(|known| {
                known
                    .collections
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
            })
    };
    let found = result.unwrap();
    assert_eq!(names(&found, "shop").unwrap(), ["orders", "users"]);
    assert_eq!(names(&found, "logs").unwrap(), ["events"]);

    let Response::Navigation(navigation) = send(&backend, Command::LoadNavigation).await else {
        panic!("unexpected response");
    };
    assert_eq!(navigation.unwrap().databases, found);
}

/// Runs a script, saying yes to its write, and returns its results.
async fn run_script(
    backend: &Backend,
    namespace: Namespace,
    script: &str,
) -> (nordpil_shell::Outcome, Vec<nordpil_shell::Event>) {
    let run = nordpil_backend::ScriptRun::new();
    let answering = std::sync::Arc::clone(&run);
    let answer = tokio::spawn(async move {
        loop {
            if answering.pending_write().is_some() {
                answering.answer(true);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let command = Command::RunScript {
        namespace,
        script: script.to_owned(),
        run: std::sync::Arc::clone(&run),
    };
    let Response::ScriptFinished(outcome) = send(backend, command).await else {
        panic!("unexpected response");
    };
    answer.abort();
    (outcome.unwrap(), run.take_events())
}

fn results(events: Vec<nordpil_shell::Event>) -> Vec<nordpil_shell::ResultValue> {
    events
        .into_iter()
        .filter_map(|event| match event {
            nordpil_shell::Event::Result { value, .. } => Some(value),
            nordpil_shell::Event::Output(_) => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn runs_scripts_and_pages_their_results() {
    use nordpil_shell::{Outcome, ResultValue};

    let (_mongo, backend, id, _) = connected_backend(false).await;
    let namespace = Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: "items".to_owned(),
    };

    let (outcome, events) = run_script(
        &backend,
        namespace.clone(),
        "db.items.insertMany([{ _id: 1, n: 'a' }, { _id: 2, n: 'b' }, { _id: 3, n: 'c' }])\n\
         db.items.find({}, { n: 1 }).sort({ _id: -1 }).limit(2).toArray()\n\
         db.items.aggregate([{ $match: { _id: { $gt: 1 } } }])\n\
         db.items.countDocuments()",
    )
    .await;

    assert_eq!(outcome, Outcome::Finished);
    let results = results(events);
    assert_eq!(
        results[1],
        ResultValue::Documents(vec![
            doc! { "_id": 3, "n": "c" },
            doc! { "_id": 2, "n": "b" }
        ])
    );
    assert_eq!(
        results[3],
        ResultValue::Value(mongodb::bson::Bson::Int32(3))
    );
    let ResultValue::Cursor(query) = &results[2] else {
        panic!("not a cursor: {:?}", results[2]);
    };
    let nordpil_shell::QueryKind::Aggregate { pipeline, options } = &query.kind else {
        panic!("not an aggregation");
    };

    let page = Command::Aggregate {
        namespace: namespace.clone(),
        query: Box::new(nordpil_backend::AggregateQuery {
            pipeline: pipeline.clone(),
            options: options.clone(),
            skip: 1,
            limit: 1,
        }),
    };
    let Response::Documents(found) = send(&backend, page).await else {
        panic!("unexpected response");
    };
    let found = found.unwrap();
    assert_eq!(found, [doc! { "_id": 3, "n": "c" }]);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn scripts_on_read_only_connections_cannot_write() {
    let (_mongo, backend, id, _) = connected_backend(true).await;
    let namespace = Namespace {
        connection: id,
        database: "shop".to_owned(),
        collection: "items".to_owned(),
    };

    let (outcome, _) = run_script(&backend, namespace, "db.items.insertOne({ a: 1 })").await;

    let nordpil_shell::Outcome::Failed(error) = outcome else {
        panic!("expected a failure, got {outcome:?}");
    };
    assert_eq!(error.message, "This connection is read-only.");
    assert!(collection_names(&backend, id, "shop").await.is_empty());
}

/// A filter that matches nothing and sleeps 50 ms on every document, so a
/// scan of [`slow_collection`] keeps the server busy for over a minute.
const SLOW_FILTER: &str = "function () { sleep(50); return false; }";

/// A collection that [`SLOW_FILTER`] takes about 100 seconds to scan.
async fn slow_collection(uri: &str, connection: Uuid) -> (mongodb::Client, Namespace) {
    let client = mongodb::Client::with_uri_str(uri).await.unwrap();
    client
        .database("shop")
        .collection::<Document>("slow")
        .insert_many((0..2000).map(|n| doc! { "n": n }))
        .await
        .unwrap();
    let namespace = Namespace {
        connection,
        database: "shop".to_owned(),
        collection: "slow".to_owned(),
    };
    (client, namespace)
}

/// How many operations on `shop.slow` the server is running.
async fn running_on_server(client: &mongodb::Client) -> usize {
    let operations: Vec<Document> = client
        .database("admin")
        .aggregate([
            doc! { "$currentOp": {} },
            doc! { "$match": { "ns": "shop.slow" } },
        ])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    operations.len()
}

/// Waits until the server runs `expected` operations on `shop.slow`, failing
/// after `limit`.
async fn wait_for_running(client: &mongodb::Client, expected: usize, limit: Duration) {
    let started = Instant::now();
    loop {
        let running = running_on_server(client).await;
        if running == expected {
            return;
        }
        assert!(
            started.elapsed() < limit,
            "{running} operations still running after {limit:?}, expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Starts `command`, waits until the server runs it, then drops it as the
/// UI's Stop does, and checks that the server stops too.
async fn stop_once_running(backend: &Backend, client: &mongodb::Client, command: Command) {
    let response = send(backend, command);
    tokio::select! {
        response = response => panic!("the query ended by itself: {response:?}"),
        () = wait_for_running(client, 1, Duration::from_secs(10)) => {}
    }
    // Without the kill, the scan would go on for about 100 seconds.
    wait_for_running(client, 0, Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn stopping_a_find_stops_it_on_the_server() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = slow_collection(&uri, id).await;

    let find = Command::Find {
        namespace,
        query: Box::new(FindQuery {
            filter: doc! { "$where": SLOW_FILTER },
            limit: 20,
            ..FindQuery::default()
        }),
    };
    stop_once_running(&backend, &client, find).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn stopping_a_count_stops_it_on_the_server() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = slow_collection(&uri, id).await;

    let count = Command::Count {
        namespace,
        // Counting runs as an aggregation, where `$where` isn't allowed.
        filter: doc! { "$expr": { "$function": { "body": SLOW_FILTER, "args": [], "lang": "js" } } },
        hint: None,
    };
    stop_once_running(&backend, &client, count).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn stopping_a_script_stops_its_query_on_the_server() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = slow_collection(&uri, id).await;

    let cancel = CancellationToken::new();
    let run = Command::RunScript {
        namespace,
        script: format!("db.slow.find({{ $where: {SLOW_FILTER:?} }}).toArray()"),
        run: nordpil_backend::ScriptRun::new(),
    };
    let (response, ()) = tokio::join!(backend.handle(run, cancel.clone()), async {
        wait_for_running(&client, 1, Duration::from_secs(10)).await;
        cancel.cancel();
    });
    let Response::ScriptFinished(outcome) = response else {
        panic!("unexpected response");
    };
    assert_eq!(outcome.unwrap(), nordpil_shell::Outcome::Cancelled);
    wait_for_running(&client, 0, Duration::from_secs(3)).await;
}

fn pipeline(stages: Vec<Document>) -> PipelineQuery {
    PipelineQuery {
        stages,
        ..PipelineQuery::default()
    }
}

/// `shop.items` with documents `{ n: 0 }` to `{ n: count - 1 }`.
async fn numbered_items(uri: &str, connection: Uuid, count: i32) -> (mongodb::Client, Namespace) {
    let client = mongodb::Client::with_uri_str(uri).await.unwrap();
    client
        .database("shop")
        .collection::<Document>("items")
        .insert_many((0..count).map(|n| doc! { "n": n }))
        .await
        .unwrap();
    let namespace = Namespace {
        connection,
        database: "shop".to_owned(),
        collection: "items".to_owned(),
    };
    (client, namespace)
}

async fn aggregate_page(
    backend: &Backend,
    namespace: &Namespace,
    key: u64,
    stages: &[Document],
    offset: u64,
) -> AggregatePage {
    let page = Command::AggregatePage {
        namespace: namespace.clone(),
        key,
        query: Box::new(pipeline(stages.to_vec())),
        offset,
        count: 10,
    };
    let Response::AggregatePage(result) = send(backend, page).await else {
        panic!("unexpected response");
    };
    result.unwrap()
}

fn numbers(page: &AggregatePage) -> Vec<i32> {
    page.documents
        .iter()
        .map(|document| document.get_i32("n").unwrap())
        .collect()
}

/// The ids of the server's idle cursors on `shop.items`.
async fn idle_cursors(client: &mongodb::Client) -> Vec<i64> {
    let cursors: Vec<Document> = client
        .database("admin")
        .aggregate([
            doc! { "$currentOp": { "idleCursors": true } },
            doc! { "$match": { "type": "idleCursor", "ns": "shop.items" } },
        ])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    cursors
        .iter()
        .map(|op| {
            op.get_document("cursor")
                .unwrap()
                .get_i64("cursorId")
                .unwrap()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn pages_an_aggregation_from_its_open_cursor() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = numbered_items(&uri, id, 25).await;
    let stages = [
        doc! { "$match": { "n": { "$gte": 0 } } },
        doc! { "$sort": { "n": 1 } },
    ];

    let first = aggregate_page(&backend, &namespace, 1, &stages, 0).await;
    assert_eq!(numbers(&first), (0..10).collect::<Vec<_>>());
    assert!(!first.exhausted);
    let open = idle_cursors(&client).await;
    assert_eq!(open.len(), 1);

    let second = aggregate_page(&backend, &namespace, 1, &stages, 10).await;
    assert_eq!(numbers(&second), (10..20).collect::<Vec<_>>());
    // The same cursor went on.
    assert_eq!(idle_cursors(&client).await, open);

    let last = aggregate_page(&backend, &namespace, 1, &stages, 20).await;
    assert_eq!(numbers(&last), (20..25).collect::<Vec<_>>());
    assert!(last.exhausted);
    assert!(idle_cursors(&client).await.is_empty());

    // A page the cursor isn't at runs the pipeline again from there.
    let again = aggregate_page(&backend, &namespace, 1, &stages, 5).await;
    assert_eq!(numbers(&again), (5..15).collect::<Vec<_>>());
    send(&backend, Command::CloseCursor(1)).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !idle_cursors(&client).await.is_empty() {
        assert!(Instant::now() < deadline, "the cursor is still open");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn resumes_when_the_server_dropped_the_cursor() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = numbered_items(&uri, id, 25).await;
    let stages = [doc! { "$sort": { "n": 1 } }];

    aggregate_page(&backend, &namespace, 7, &stages, 0).await;
    let open = idle_cursors(&client).await;
    assert_eq!(open.len(), 1);
    client
        .database("shop")
        .run_command(doc! { "killCursors": "items", "cursors": open })
        .await
        .unwrap();

    let next = aggregate_page(&backend, &namespace, 7, &stages, 10).await;
    assert_eq!(numbers(&next), (10..20).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn counts_explains_and_writes_aggregations() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = numbered_items(&uri, id, 25).await;

    for (stages, expected) in [
        (vec![], 25),
        (vec![doc! { "$match": { "n": { "$lt": 5 } } }], 5),
        (vec![doc! { "$match": { "n": -1 } }], 0),
    ] {
        let count = Command::CountAggregate {
            namespace: namespace.clone(),
            query: Box::new(pipeline(stages)),
        };
        let Response::Count(result) = send(&backend, count).await else {
            panic!("unexpected response");
        };
        assert_eq!(result.unwrap(), Some(expected));
    }

    let explain = Command::ExplainAggregate {
        namespace: namespace.clone(),
        query: Box::new(pipeline(vec![doc! { "$match": { "n": 3 } }])),
    };
    let Response::Explained(result) = send(&backend, explain).await else {
        panic!("unexpected response");
    };
    let plan = result.unwrap();
    assert!(plan.contains_key("queryPlanner") || plan.contains_key("stages"));

    let writing = vec![
        doc! { "$match": { "n": { "$lt": 3 } } },
        doc! { "$out": "copy" },
    ];
    let page = Command::AggregatePage {
        namespace: namespace.clone(),
        key: 1,
        query: Box::new(pipeline(writing.clone())),
        offset: 0,
        count: 10,
    };
    let Response::AggregatePage(result) = send(&backend, page).await else {
        panic!("unexpected response");
    };
    assert!(result.unwrap_err().message.contains("$out or $merge"));
    assert!(
        !collection_names(&backend, id, "shop")
            .await
            .contains(&"copy".to_owned())
    );

    let dir = std::env::temp_dir().join(format!("nordpil-it-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let export = |query, file: &str| {
        Command::Export(Box::new(nordpil_backend::transfer::ExportJob {
            namespace: namespace.clone(),
            query: nordpil_backend::transfer::ExportQuery::Pipeline(Box::new(query)),
            path: dir.join(file),
            format: nordpil_backend::transfer::Format::Ndjson,
            json_mode: nordpil_backend::transfer::JsonMode::Relaxed,
            fields: Vec::new(),
            progress: std::sync::Arc::default(),
        }))
    };
    let sorted = pipeline(vec![
        doc! { "$match": { "n": { "$gte": 20 } } },
        doc! { "$project": { "_id": 0, "n": 1 } },
        doc! { "$sort": { "n": 1 } },
    ]);
    let Response::Exported(result) = send(&backend, export(sorted, "out.ndjson")).await else {
        panic!("unexpected response");
    };
    assert_eq!(result.unwrap(), 5);
    let text = std::fs::read_to_string(dir.join("out.ndjson")).unwrap();
    assert_eq!(text.lines().next(), Some(r#"{"n":20}"#));
    let Response::Exported(result) =
        send(&backend, export(pipeline(writing.clone()), "no.ndjson")).await
    else {
        panic!("unexpected response");
    };
    assert!(result.unwrap_err().message.contains("$out or $merge"));
    assert!(
        !collection_names(&backend, id, "shop")
            .await
            .contains(&"copy".to_owned())
    );

    let write = Command::WriteAggregate {
        namespace,
        query: Box::new(pipeline(writing)),
    };
    let Response::AggregateWritten(result) = send(&backend, write).await else {
        panic!("unexpected response");
    };
    result.unwrap();
    let copied = client
        .database("shop")
        .collection::<Document>("copy")
        .count_documents(doc! {})
        .await
        .unwrap();
    assert_eq!(copied, 3);
}

/// A stage that sleeps 50 ms on every document of [`slow_collection`].
fn slow_stage() -> Document {
    doc! { "$match": { "$expr": { "$function": { "body": SLOW_FILTER, "args": [], "lang": "js" } } } }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn stopping_an_aggregation_stops_it_on_the_server() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (client, namespace) = slow_collection(&uri, id).await;

    let page = Command::AggregatePage {
        namespace: namespace.clone(),
        key: 1,
        query: Box::new(pipeline(vec![slow_stage()])),
        offset: 0,
        count: 20,
    };
    stop_once_running(&backend, &client, page).await;

    let count = Command::CountAggregate {
        namespace,
        query: Box::new(pipeline(vec![slow_stage()])),
    };
    stop_once_running(&backend, &client, count).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn a_slow_aggregate_count_gives_up() {
    let (_mongo, backend, id, uri) = connected_backend(false).await;
    let (_client, namespace) = slow_collection(&uri, id).await;

    let count = Command::CountAggregate {
        namespace,
        query: Box::new(pipeline(vec![slow_stage()])),
    };
    let Response::Count(result) = send(&backend, count).await else {
        panic!("unexpected response");
    };
    assert_eq!(result.unwrap(), None);
}
