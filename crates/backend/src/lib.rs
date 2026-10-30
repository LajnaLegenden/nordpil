//! Background runtime and database access for nordpil.

mod bridge;

pub use bridge::{Bridge, Event, Handler, Outcome, RequestId};
pub use tokio_util::sync::CancellationToken;

/// Commands the UI sends to the backend.
#[derive(Debug)]
pub enum Command {}

/// Results of [`Command`]s.
#[derive(Debug)]
pub enum Response {}

/// Handles [`Command`]s on the background runtime.
#[derive(Default)]
pub struct Backend {}

impl Handler for Backend {
    type Command = Command;
    type Response = Response;

    async fn handle(&self, command: Command, _cancel: CancellationToken) -> Response {
        match command {}
    }
}
