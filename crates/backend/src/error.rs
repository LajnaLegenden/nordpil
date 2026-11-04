//! Errors with messages written for the people using the app.

use std::fmt;

use mongodb::error::ErrorKind as DriverErrorKind;

/// Lets the UI react to specific failures, e.g. ask for a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The connection has a username but no password is known.
    PasswordRequired,
    AuthenticationFailed,
    ReadOnly,
    /// The document changed or was deleted since it was loaded.
    Conflict,
    /// The query ran out of its time limit (`maxTimeMS`).
    TimedOut,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError {
    pub kind: ErrorKind,
    pub message: String,
    /// Raw technical detail, e.g. the driver's error text.
    pub detail: Option<String>,
}

impl BackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_detail(mut self, detail: impl fmt::Display) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    pub(crate) fn read_only() -> Self {
        Self::new("This connection is read-only.").with_kind(ErrorKind::ReadOnly)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BackendError {}

impl From<mongodb::error::Error> for BackendError {
    fn from(error: mongodb::error::Error) -> Self {
        let (kind, message) = match error.kind.as_ref() {
            DriverErrorKind::ServerSelection { .. } => (
                ErrorKind::Other,
                "Could not reach the server. Check the host, port and network, \
                 and that the server is running."
                    .to_owned(),
            ),
            DriverErrorKind::Authentication { .. } => (
                ErrorKind::AuthenticationFailed,
                "Authentication failed. Check the username, password and auth database.".to_owned(),
            ),
            DriverErrorKind::DnsResolve { message, .. } => {
                (ErrorKind::Other, format!("DNS lookup failed: {message}"))
            }
            DriverErrorKind::InvalidArgument { message, .. } => (
                ErrorKind::Other,
                format!("Invalid connection string: {message}"),
            ),
            DriverErrorKind::InvalidTlsConfig { message, .. } => (
                ErrorKind::Other,
                format!("TLS configuration error: {message}"),
            ),
            DriverErrorKind::Command(command) if command.code == UNAUTHORIZED => (
                ErrorKind::Other,
                format!("Not authorized: {}", command.message),
            ),
            DriverErrorKind::Command(command) if command.code == MAX_TIME_EXPIRED => {
                (ErrorKind::TimedOut, command.message.clone())
            }
            DriverErrorKind::Command(command) => (ErrorKind::Other, command.message.clone()),
            DriverErrorKind::Io(io) => (ErrorKind::Other, format!("Network error: {io}")),
            _ => (ErrorKind::Other, error.to_string()),
        };
        Self::new(message).with_kind(kind).with_detail(&error)
    }
}

/// Server error code for a missing privilege.
const UNAUTHORIZED: i32 = 13;
/// Server error code when `maxTimeMS` runs out.
const MAX_TIME_EXPIRED: i32 = 50;
