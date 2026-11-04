//! Runs commands on a background tokio runtime and hands the results back to
//! the UI thread without blocking it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::{Handle, Runtime};
use tokio_util::sync::CancellationToken;

/// Identifies one command sent through a [`Bridge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

impl RequestId {
    /// For tests that stand in for a bridge.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Executes commands on the background runtime.
pub trait Handler: Send + Sync + 'static {
    type Command: Send + 'static;
    type Response: Send + 'static;

    /// Handles one command. The returned future is dropped when the request
    /// is cancelled; long-running work can also watch `cancel` to stop cleanly.
    fn handle(
        &self,
        command: Self::Command,
        cancel: CancellationToken,
    ) -> impl Future<Output = Self::Response> + Send;
}

/// The result of one request.
#[derive(Debug)]
pub struct Event<R> {
    pub id: RequestId,
    pub outcome: Outcome<R>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<R> {
    Done(R),
    Cancelled,
}

type InFlight = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

/// Owns the background runtime. The UI sends commands with [`Bridge::send`]
/// and collects results with [`Bridge::drain`] once per frame.
pub struct Bridge<H: Handler> {
    runtime: Option<Runtime>,
    handle: Handle,
    handler: Arc<H>,
    events_tx: Sender<Event<H::Response>>,
    events_rx: Receiver<Event<H::Response>>,
    in_flight: InFlight,
    next_id: u64,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl<H: Handler> Bridge<H> {
    /// `wake` runs on a background thread after each event is queued. The UI
    /// uses it to request a repaint, so it drains the event promptly.
    pub fn new(handler: H, wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        // Database work is I/O-bound, so two workers are plenty; blocking work
        // (keyring, files) runs on tokio's separate blocking pool.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("nordpil-backend")
            .build()?;
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        Ok(Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            handler: Arc::new(handler),
            events_tx,
            events_rx,
            in_flight: InFlight::default(),
            next_id: 0,
            wake: Arc::new(wake),
        })
    }

    /// Starts a command and returns its id. The result arrives later through
    /// [`Bridge::drain`].
    pub fn send(&mut self, command: H::Command) -> RequestId {
        self.next_id += 1;
        let id = RequestId(self.next_id);
        let cancel = CancellationToken::new();
        lock(&self.in_flight).insert(id, cancel.clone());

        let handler = Arc::clone(&self.handler);
        let in_flight = Arc::clone(&self.in_flight);
        let events_tx = self.events_tx.clone();
        let wake = Arc::clone(&self.wake);
        self.handle.spawn(async move {
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => Outcome::Cancelled,
                response = handler.handle(command, cancel.clone()) => Outcome::Done(response),
            };
            lock(&in_flight).remove(&id);
            // The receiver lives as long as the bridge, so this only fails during shutdown.
            if events_tx.send(Event { id, outcome }).is_ok() {
                wake();
            }
        });
        id
    }

    /// Cancels a request. Its event arrives as [`Outcome::Cancelled`].
    /// Does nothing if the request has already finished.
    pub fn cancel(&self, id: RequestId) {
        if let Some(token) = lock(&self.in_flight).get(&id) {
            token.cancel();
        }
    }

    /// Returns the events that arrived since the last call, without blocking.
    pub fn drain(&self) -> impl Iterator<Item = Event<H::Response>> + '_ {
        self.events_rx.try_iter()
    }
}

impl<H: Handler> Drop for Bridge<H> {
    fn drop(&mut self) {
        // Don't block the closing window on in-flight work.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn lock(in_flight: &InFlight) -> MutexGuard<'_, HashMap<RequestId, CancellationToken>> {
    // No code panics while holding the lock, so a poisoned map is still consistent.
    in_flight.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    enum TestCommand {
        Double(u32),
        Hang {
            started: Sender<()>,
            dropped: Arc<AtomicBool>,
        },
    }

    struct TestHandler;

    impl Handler for TestHandler {
        type Command = TestCommand;
        type Response = u32;

        async fn handle(&self, command: TestCommand, _cancel: CancellationToken) -> u32 {
            match command {
                TestCommand::Double(n) => n * 2,
                TestCommand::Hang { started, dropped } => {
                    let _guard = SetOnDrop(dropped);
                    started.send(()).unwrap();
                    std::future::pending().await
                }
            }
        }
    }

    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn bridge() -> (Bridge<TestHandler>, Receiver<()>) {
        let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
        let bridge = Bridge::new(TestHandler, move || {
            let _ = wake_tx.send(());
        })
        .unwrap();
        (bridge, wake_rx)
    }

    #[test]
    fn wakes_ui_and_delivers_response() {
        let (mut bridge, wake_rx) = bridge();

        let id = bridge.send(TestCommand::Double(21));

        wake_rx.recv_timeout(TIMEOUT).unwrap();
        let events: Vec<_> = bridge.drain().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].outcome, Outcome::Done(42));
    }

    #[test]
    fn cancel_drops_handler_future_and_reports_cancelled() {
        let (mut bridge, wake_rx) = bridge();
        let (started_tx, started_rx) = crossbeam_channel::unbounded();
        let dropped = Arc::new(AtomicBool::new(false));

        let id = bridge.send(TestCommand::Hang {
            started: started_tx,
            dropped: Arc::clone(&dropped),
        });
        started_rx.recv_timeout(TIMEOUT).unwrap();
        bridge.cancel(id);

        wake_rx.recv_timeout(TIMEOUT).unwrap();
        let event = bridge.drain().next().unwrap();
        assert_eq!(event.id, id);
        assert_eq!(event.outcome, Outcome::Cancelled);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(lock(&bridge.in_flight).is_empty());
    }
}
