//! Panels and dialogs. They don't change app state directly; they return
//! [`Action`]s that [`crate::app::NordpilApp`] applies after drawing.

pub mod chrome;
pub mod connection_form;
pub mod dialogs;
pub mod document_dialogs;
pub mod documents;
pub mod goto;
pub mod index_dialog;
pub mod indexes;
pub mod mappings_dialog;
pub mod settings_dialog;
pub mod shell;
pub mod shortcuts_dialog;
pub mod sidebar;
pub mod transfer_dialog;

use nordpil_backend::Password;
use uuid::Uuid;

use crate::connections::Selection;

pub enum Action {
    NewConnection,
    EditConnection(Uuid),
    /// Asks for confirmation first.
    DeleteConnection(Uuid),
    DeleteConnectionConfirmed(Uuid),
    Connect(Uuid),
    ConnectWithPassword {
        id: Uuid,
        password: Password,
        remember: bool,
    },
    CancelConnect(Uuid),
    Disconnect(Uuid),
    RefreshDatabases(Uuid),
    LoadCollections {
        connection: Uuid,
        database: String,
    },
    Select(Selection),
    /// Opens the create dialog. Without a database, the dialog asks for one.
    CreateCollection {
        connection: Uuid,
        database: Option<String>,
    },
    CreateCollectionConfirmed {
        connection: Uuid,
        database: String,
        collection: String,
    },
    /// Asks for confirmation first. Without a collection, drops the database.
    Drop {
        connection: Uuid,
        database: String,
        collection: Option<String>,
    },
    DropConfirmed {
        connection: Uuid,
        database: String,
        collection: Option<String>,
    },
    /// Opens the database's linked fields, for lookups.
    EditMappings {
        connection: Uuid,
        database: String,
    },
}
