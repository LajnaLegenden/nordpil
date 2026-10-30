//! Connection passwords, kept in the OS keyring.

use uuid::Uuid;

use crate::BackendError;

/// Where connection passwords live. Calls may block (e.g. on D-Bus), so the
/// backend only calls them from blocking tasks.
pub trait SecretStore: Send + Sync + 'static {
    /// False when no keyring is available. Passwords are then asked for on
    /// each connect.
    fn available(&self) -> bool;
    fn get(&self, id: Uuid) -> Result<Option<String>, BackendError>;
    fn set(&self, id: Uuid, password: &str) -> Result<(), BackendError>;
    /// Succeeds when there is nothing to delete.
    fn delete(&self, id: Uuid) -> Result<(), BackendError>;
}

/// The platform keyring: Keychain on macOS, Credential Manager on Windows,
/// Secret Service elsewhere.
pub struct Keyring;

const SERVICE: &str = "nordpil";

impl Keyring {
    fn entry(id: Uuid) -> Result<keyring::Entry, BackendError> {
        keyring::Entry::new(SERVICE, &id.to_string()).map_err(keyring_error)
    }
}

impl SecretStore for Keyring {
    fn available(&self) -> bool {
        keyring::Entry::store_status().is_ok()
    }

    fn get(&self, id: Uuid) -> Result<Option<String>, BackendError> {
        match Self::entry(id)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn set(&self, id: Uuid, password: &str) -> Result<(), BackendError> {
        Self::entry(id)?
            .set_password(password)
            .map_err(keyring_error)
    }

    fn delete(&self, id: Uuid) -> Result<(), BackendError> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_error(error)),
        }
    }
}

fn keyring_error(error: keyring::Error) -> BackendError {
    BackendError::new("Could not access the system keyring.").with_detail(error)
}
