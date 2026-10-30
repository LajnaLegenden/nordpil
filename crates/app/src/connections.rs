//! UI-side state of saved and open connections.

use nordpil_backend::store::SavedConnection;
use nordpil_backend::{BackendError, CollectionInfo, DatabaseInfo, RequestId};
use uuid::Uuid;

pub struct ConnectionEntry {
    pub saved: SavedConnection,
    pub state: ConnectionState,
}

pub enum ConnectionState {
    Disconnected,
    Connecting(RequestId),
    Connected(Vec<DatabaseNode>),
    Failed(BackendError),
}

pub struct DatabaseNode {
    pub info: DatabaseInfo,
    pub collections: Collections,
}

pub enum Collections {
    NotLoaded,
    Loading,
    Loaded(Vec<CollectionInfo>),
    Failed(BackendError),
}

/// The collection shown in the main panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
}

impl ConnectionEntry {
    pub fn new(saved: SavedConnection) -> Self {
        Self {
            saved,
            state: ConnectionState::Disconnected,
        }
    }

    pub fn database_mut(&mut self, name: &str) -> Option<&mut DatabaseNode> {
        match &mut self.state {
            ConnectionState::Connected(databases) => {
                databases.iter_mut().find(|db| db.info.name == name)
            }
            _ => None,
        }
    }

    /// Replaces the database list, keeping collections already loaded for
    /// databases that still exist.
    pub fn set_databases(&mut self, databases: Vec<DatabaseInfo>) {
        let mut old = match std::mem::replace(&mut self.state, ConnectionState::Disconnected) {
            ConnectionState::Connected(old) => old,
            _ => Vec::new(),
        };
        let nodes = databases
            .into_iter()
            .map(|info| {
                let collections = old
                    .iter()
                    .position(|node| node.info.name == info.name)
                    .map_or(Collections::NotLoaded, |index| {
                        old.swap_remove(index).collections
                    });
                DatabaseNode { info, collections }
            })
            .collect();
        self.state = ConnectionState::Connected(nodes);
    }
}
