//! Runs against a real mongod in Docker. Run with `just test-int`.

use mongodb::bson::{Document, doc};
use nordpil_backend::secrets::MemorySecrets;
use nordpil_backend::store::{ConnectionStore, SavedConnection};
use nordpil_backend::{
    Backend, CancellationToken, Command, ErrorKind, FindQuery, Handler, Namespace, PasswordChange,
    Response,
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
    assert_eq!(
        result.unwrap(),
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
