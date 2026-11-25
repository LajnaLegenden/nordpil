use std::time::Duration;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(5);

struct HangHandler;

impl Handler for HangHandler {
    type Command = Sender<()>;
    type Response = ();

    async fn handle(&self, started: Sender<()>, _cancel: CancellationToken) {
        started.send(()).unwrap();
        std::future::pending().await
    }
}

#[test]
fn is_busy_while_a_command_runs() {
    let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
    let mut bridge = Bridge::new(HangHandler, move || {
        let _ = wake_tx.send(());
    })
    .unwrap();
    let (started_tx, started_rx) = crossbeam_channel::unbounded();
    assert!(!bridge.is_busy());

    let id = bridge.send(started_tx);
    started_rx.recv_timeout(TIMEOUT).unwrap();
    assert!(bridge.is_busy());

    bridge.cancel(id);
    wake_rx.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(bridge.drain().count(), 1);
    assert!(!bridge.is_busy());
}
