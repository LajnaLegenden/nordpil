use tokio_util::sync::CancellationToken;

use super::*;
use crate::secrets::MemorySecrets;

#[tokio::test]
async fn disconnect_all_closes_every_session() {
    let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
    let backend = Backend::new(
        ConnectionStore::new(dir.join("connections.toml")),
        MemorySecrets::default(),
    );
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    for id in ids {
        let client = Client::with_uri_str("mongodb://localhost:1").await.unwrap();
        backend.lock_sessions().insert(
            id,
            Session {
                client,
                read_only: false,
            },
        );
    }

    let response = backend
        .handle(Command::DisconnectAll, CancellationToken::new())
        .await;
    assert!(matches!(response, Response::Stored(Ok(()))));

    for connection in ids {
        let response = backend
            .handle(Command::ListDatabases(connection), CancellationToken::new())
            .await;
        let Response::Databases { result, .. } = response else {
            panic!("unexpected response");
        };
        assert_eq!(result.unwrap_err().message, "Not connected.");
    }
}
