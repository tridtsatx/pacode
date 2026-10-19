//! Event forwarding from a core broadcast into one connection's writer queue.
//!
//! Events used to vanish silently when the broadcast ring lagged or the per-connection
//! queue was full. Now every loss marks the forwarder "desynced"; before the next event
//! it sends one [`Event::Resync`] (seq 0) so the client re-fetches a snapshot.
//! Owner: one forwarder task per (connection, broadcast). State is a single `bool`
//! local to that task, so nothing is shared and nothing can grow.

use pacode_types::protocol::{Event, ServerMessage};
use tokio::sync::{broadcast, mpsc};

fn encode(seq: u64, event: Event) -> Option<String> {
    match serde_json::to_string(&ServerMessage::Event { seq, event }) {
        Ok(mut s) => {
            s.push('\n');
            Some(s)
        }
        Err(err) => {
            log::error!("failed to serialize event: {err}");
            None
        }
    }
}

/// Forward `rx` into `tx` until the broadcast closes or the connection's queue is gone.
/// `label` names the stream in logs ("session" or "global").
pub async fn run_forwarder(
    mut rx: broadcast::Receiver<(u64, Event)>,
    tx: mpsc::Sender<String>,
    label: &'static str,
) {
    let mut desynced = false;
    loop {
        if desynced {
            // Wait for room (the client is reading again), then tell it first.
            let Ok(permit) = tx.reserve().await else {
                break;
            };
            let reason = format!("{label} events were dropped for this connection");
            let Some(line) = encode(0, Event::Resync { reason }) else {
                break;
            };
            permit.send(line);
            desynced = false;
        }
        match rx.recv().await {
            Ok((seq, event)) => {
                if let Some(line) = encode(seq, event) {
                    match tx.try_send(line) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => desynced = true,
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!("{label} event receiver lagged by {n} events");
                desynced = true;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
