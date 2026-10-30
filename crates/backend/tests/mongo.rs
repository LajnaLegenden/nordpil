//! Runs against a real mongod in Docker. Run with `just test-int`.

use std::collections::HashMap;
use std::sync::Mutex;

use nordpil_backend::secrets::SecretStore;
use nordpil_backend::store::{ConnectionStore, SavedConnection};
use nordpil_backend::{
    Backend, BackendError, CancellationToken, Command, ErrorKind, Handler, PasswordChange, Response,
};
use testcontainers_modules::mongo::Mongo;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use uuid::Uuid;

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<Uuid, String>>);

impl SecretStore for MemorySecrets {
    fn available(&self) -> bool {
        true
    }

    fn get(&self, id: Uuid) -> Result<Option<String>, BackendError> {
        Ok(self.0.lock().unwrap().get(&id).cloned())
    }

    fn set(&self, id: Uuid, password: &str) -> Result<(), BackendError> {
        self.0.lock().unwrap().insert(id, password.to_owned());
        Ok(())
    }

    fn delete(&self, id: Uuid) -> Result<(), BackendError> {
        self.0.lock().unwrap().remove(&id);
        Ok(())
    }
}

async fn send(backend: &Backend, command: Command) -> Response {
    backend.handle(command, CancellationToken::new()).await
}

/// Starts mongod and returns a backend connected to it.
async fn connected_backend(read_only: bool) -> (ContainerAsync<Mongo>, Backend, Uuid) {
    let mongo = Mongo::default().with_tag("8.0").start().await.unwrap();
    let port = mongo.get_host_port_ipv4(27017).await.unwrap();
    let dir = std::env::temp_dir().join(format!("nordpil-it-{}", Uuid::new_v4()));
    let backend = Backend::new(
        ConnectionStore::new(dir.join("connections.toml")),
        MemorySecrets::default(),
    );

    let id = Uuid::new_v4();
    let connection = SavedConnection {
        id,
        name: "test".to_owned(),
        uri: format!("mongodb://127.0.0.1:{port}/?directConnection=true"),
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
    (mongo, backend, id)
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
    let (_mongo, backend, id) = connected_backend(false).await;

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
    let (_mongo, backend, id) = connected_backend(true).await;

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
