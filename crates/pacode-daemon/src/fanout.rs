//! Event forwarding from a core broadcast into one connection's writer queue.
//!
//! Events used to vanish silently when the broadcast ring lagged or the per-connection
//! queue was full. Now every loss marks the forwarder "desynced"; before the next event
//! it sends one [`Event::Resync`] (seq 0) so the client re-fetches a snapshot.
//!
//! Interest filtering (`Request::Subscribe`, see [`crate::interest`]): the session forwarder
//! drops `TextDelta`/`ReasoningDelta`/`ItemAdded`/`ItemUpdated` of agents the connection did
//! not subscribe to, and coalesces their `AgentUpdated` (latest per agent) to at most one
//! flush per [`COALESCE_INTERVAL`]. The flush timer exists only while something is pending.
//! Filtering leaves holes in `seq`; loss is still reported by `Resync`, never by a gap.
//!
//! Owner: one forwarder task per (connection, broadcast). State is a `bool` plus the
//! pending map, which holds at most one entry per agent of the session (cleared on flush).

use std::collections::HashMap;
use std::time::Duration;

use pacode_types::protocol::{Event, ServerMessage};
use pacode_types::{AgentId, AgentInfo};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

use crate::interest::{self, SharedInterest};

/// Minimum spacing between flushes of coalesced `AgentUpdated` (2 Hz).
pub const COALESCE_INTERVAL: Duration = Duration::from_millis(500);

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

/// What the interest filter decides for one event.
enum Routed {
    Send(Event),
    Skip,
    /// `AgentUpdated` of an unsubscribed agent: keep the latest, flush later.
    Hold(AgentInfo),
}

fn route(interest: &SharedInterest, event: Event) -> Routed {
    let wanted = match &event {
        Event::TextDelta { agent, .. } | Event::ReasoningDelta { agent, .. } => {
            Some(interest::wants(interest, agent))
        }
        Event::ItemAdded(item) | Event::ItemUpdated(item) => {
            Some(interest::wants(interest, &item.agent))
        }
        Event::AgentUpdated(info) => Some(interest::wants(interest, &info.id)),
        _ => None,
    };
    match (wanted, event) {
        (None, event) | (Some(true), event) => Routed::Send(event),
        (Some(false), Event::AgentUpdated(info)) => Routed::Hold(info),
        (Some(false), _) => Routed::Skip,
    }
}

/// Queue one line. Returns false when the connection is gone.
fn deliver(tx: &mpsc::Sender<String>, seq: u64, event: Event, desynced: &mut bool) -> bool {
    let Some(line) = encode(seq, event) else {
        return true;
    };
    match tx.try_send(line) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            *desynced = true;
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Forward `rx` into `tx` until the broadcast closes or the connection's queue is gone.
/// `label` names the stream in logs ("session" or "global").
pub async fn run_forwarder(
    rx: broadcast::Receiver<(u64, Event)>,
    tx: mpsc::Sender<String>,
    label: &'static str,
) {
    forward(rx, tx, label, None).await;
}

/// [`run_forwarder`] with per-agent interest filtering and `AgentUpdated` coalescing.
pub async fn run_filtered_forwarder(
    rx: broadcast::Receiver<(u64, Event)>,
    tx: mpsc::Sender<String>,
    label: &'static str,
    interest: SharedInterest,
) {
    forward(rx, tx, label, Some(interest)).await;
}

async fn forward(
    mut rx: broadcast::Receiver<(u64, Event)>,
    tx: mpsc::Sender<String>,
    label: &'static str,
    interest: Option<SharedInterest>,
) {
    let mut desynced = false;
    let mut pending: HashMap<AgentId, (u64, AgentInfo)> = HashMap::new();
    let mut flush_at: Option<Instant> = None;
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
        let deadline = flush_at;
        let flush = async move {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok((seq, event)) => {
                    let routed = match &interest {
                        Some(i) => route(i, event),
                        None => Routed::Send(event),
                    };
                    match routed {
                        Routed::Send(event) => {
                            if let Event::AgentUpdated(info) = &event {
                                // Subscribed now: a held older update would arrive after it.
                                pending.remove(&info.id);
                            }
                            if !deliver(&tx, seq, event, &mut desynced) {
                                break;
                            }
                        }
                        Routed::Skip => {}
                        Routed::Hold(info) => {
                            pending.insert(info.id.clone(), (seq, info));
                            flush_at.get_or_insert_with(|| Instant::now() + COALESCE_INTERVAL);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("{label} event receiver lagged by {n} events");
                    desynced = true;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            () = flush => {
                flush_at = None;
                let mut held: Vec<(u64, AgentInfo)> = pending.drain().map(|(_, v)| v).collect();
                held.sort_by_key(|(seq, _)| *seq);
                let mut alive = true;
                for (seq, info) in held {
                    if !deliver(&tx, seq, Event::AgentUpdated(info), &mut desynced) {
                        alive = false;
                        break;
                    }
                }
                if !alive {
                    break;
                }
            }
        }
    }
}
