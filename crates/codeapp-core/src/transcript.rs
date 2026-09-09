//! Event sink (seq + broadcast), transcript items and delta coalescing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use codeapp_types::{AgentId, CallId, Event, Message, TranscriptItem};
use tokio::sync::broadcast;

/// Per-session event fan-out with a monotonic seq.
pub struct EventSink {
    seq: AtomicU64,
    tx: broadcast::Sender<(u64, Event)>,
}

impl EventSink {
    /// Capacity 1024: a lagging client re-syncs with `GetSnapshot`.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            seq: AtomicU64::new(0),
            tx,
        }
    }

    pub fn emit(&self, event: Event) -> u64 {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let _ = self.tx.send((seq, event));
        seq
    }

    pub fn last_seq(&self) -> u64 {
        self.seq.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(u64, Event)> {
        self.tx.subscribe()
    }
}

impl Default for EventSink {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-agent transcript bookkeeping: item seqs, the live assistant/reasoning items,
/// tool-call items by call id, and a bounded tail for snapshots.
pub struct TranscriptState {
    pub next_item_seq: u64,
    pub live_assistant: Option<u64>,
    pub live_reasoning: Option<u64>,
    pub tool_items: HashMap<CallId, TranscriptItem>,
    /// Bounded (`session.history_page × 2`) tail of items for snapshots/history.
    pub tail: VecDeque<TranscriptItem>,
    pub tail_cap: usize,
}

impl TranscriptState {
    pub fn new(tail_cap: usize) -> Self {
        Self {
            next_item_seq: 0,
            live_assistant: None,
            live_reasoning: None,
            tool_items: HashMap::new(),
            tail: VecDeque::new(),
            tail_cap,
        }
    }

    pub fn next_seq(&mut self) -> u64 {
        let seq = self.next_item_seq;
        self.next_item_seq += 1;
        seq
    }

    /// Insert or replace (same seq) in the tail, evicting the oldest past the cap.
    pub fn upsert(&mut self, item: TranscriptItem) {
        let _ = item;
        todo!("TranscriptState::upsert")
    }
}

/// Convert persisted history into transcript items (used for resume and
/// `GetHistory` beyond the in-memory tail). Hidden messages are skipped; tool_use and
/// tool_result pairs become one `ToolCall` item with the final status.
pub fn history_to_items(
    agent: &AgentId,
    messages: &[(u64, Arc<Message>)],
    first_item_seq: u64,
) -> Vec<TranscriptItem> {
    let _ = (agent, messages, first_item_seq);
    todo!("transcript::history_to_items")
}

/// Coalesces text deltas: flush when ≥ `max_bytes` or `max_age` since the first
/// buffered byte. The turn loop calls `push` on each delta and `take_if_due`/`take`
/// around its stream polling (`tokio::time::timeout(max_age, next)`).
pub struct DeltaCoalescer {
    buf: String,
    since: Option<Instant>,
    pub max_bytes: usize,
    pub max_age: Duration,
}

impl DeltaCoalescer {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            since: None,
            max_bytes: 256,
            max_age: Duration::from_millis(25),
        }
    }

    pub fn push(&mut self, text: &str, now: Instant) -> Option<String> {
        if self.since.is_none() {
            self.since = Some(now);
        }
        self.buf.push_str(text);
        if self.buf.len() >= self.max_bytes {
            return self.take();
        }
        None
    }

    pub fn take_if_due(&mut self, now: Instant) -> Option<String> {
        match self.since {
            Some(since) if now.duration_since(since) >= self.max_age => self.take(),
            Some(_) | None => None,
        }
    }

    pub fn take(&mut self) -> Option<String> {
        self.since = None;
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }
}

impl Default for DeltaCoalescer {
    fn default() -> Self {
        Self::new()
    }
}
