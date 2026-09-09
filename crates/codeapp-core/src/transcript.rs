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
        let mut replaced = false;
        for existing in &mut self.tail {
            if existing.seq == item.seq {
                *existing = item.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            self.tail.push_back(item.clone());
            while self.tail.len() > self.tail_cap {
                self.tail.pop_front();
            }
        }

        match &item.kind {
            codeapp_types::TranscriptKind::Assistant { complete, .. } => {
                if !*complete {
                    self.live_assistant = Some(item.seq);
                } else if self.live_assistant == Some(item.seq) {
                    self.live_assistant = None;
                }
            }
            codeapp_types::TranscriptKind::Reasoning { complete, .. } => {
                if !*complete {
                    self.live_reasoning = Some(item.seq);
                } else if self.live_reasoning == Some(item.seq) {
                    self.live_reasoning = None;
                }
            }
            codeapp_types::TranscriptKind::ToolCall { call_id, .. } => {
                self.tool_items.insert(call_id.clone(), item);
            }
            codeapp_types::TranscriptKind::User { .. }
            | codeapp_types::TranscriptKind::Notice { .. }
            | codeapp_types::TranscriptKind::Permission(_) => {}
        }
    }
}

fn tool_title(name: &str, input: &serde_json::Value) -> String {
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            if k != "intent"
                && k != "accept_large_output"
                && let Some(s) = v.as_str()
            {
                let arg = s.trim();
                let truncated: String = arg.chars().take(60).collect();
                return format!("{name} {truncated}");
            }
        }
    }
    name.to_string()
}

/// Convert persisted history into transcript items (used for resume and
/// `GetHistory` beyond the in-memory tail). Hidden messages are skipped; tool_use and
/// tool_result pairs become one `ToolCall` item with the final status.
pub fn history_to_items(
    agent: &AgentId,
    messages: &[(u64, Arc<Message>)],
    first_item_seq: u64,
) -> Vec<TranscriptItem> {
    use codeapp_types::{ContentBlock, Role, ToolStatus, TranscriptKind};

    let mut items = Vec::new();
    let mut tool_indices: HashMap<CallId, usize> = HashMap::new();
    let mut cur_seq = first_item_seq;

    for (_msg_seq, msg) in messages {
        if msg.meta.hidden {
            continue;
        }

        match msg.role {
            Role::User => {
                let text = msg.text();
                items.push(TranscriptItem {
                    seq: cur_seq,
                    agent: agent.clone(),
                    ts_ms: msg.meta.timestamp_ms,
                    kind: TranscriptKind::User { text },
                });
                cur_seq += 1;
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Reasoning { text, .. } => {
                            if !text.is_empty() {
                                items.push(TranscriptItem {
                                    seq: cur_seq,
                                    agent: agent.clone(),
                                    ts_ms: msg.meta.timestamp_ms,
                                    kind: TranscriptKind::Reasoning {
                                        text: text.clone(),
                                        complete: true,
                                    },
                                });
                                cur_seq += 1;
                            }
                        }
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                items.push(TranscriptItem {
                                    seq: cur_seq,
                                    agent: agent.clone(),
                                    ts_ms: msg.meta.timestamp_ms,
                                    kind: TranscriptKind::Assistant {
                                        text: text.clone(),
                                        complete: true,
                                    },
                                });
                                cur_seq += 1;
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let title = tool_title(name, input);
                            let intent = codeapp_tools::intent_of(input);
                            let item_idx = items.len();
                            items.push(TranscriptItem {
                                seq: cur_seq,
                                agent: agent.clone(),
                                ts_ms: msg.meta.timestamp_ms,
                                kind: TranscriptKind::ToolCall {
                                    call_id: id.clone(),
                                    name: name.clone(),
                                    title,
                                    intent,
                                    status: ToolStatus::Running,
                                    preview: String::new(),
                                    diff: None,
                                    duration_ms: None,
                                    task: None,
                                },
                            });
                            tool_indices.insert(id.clone(), item_idx);
                            cur_seq += 1;
                        }
                        ContentBlock::ToolResult { .. } => {}
                    }
                }
            }
            Role::Tool => {
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } = block
                        && let Some(&idx) = tool_indices.get(call_id)
                        && let TranscriptKind::ToolCall {
                            ref mut status,
                            ref mut preview,
                            ..
                        } = items[idx].kind
                    {
                        *status = if *is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        };
                        *preview = codeapp_types::truncate_head_tail(content, 256);
                    }
                }
            }
            Role::System => {}
        }
    }

    items
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

#[cfg(test)]
mod tests {
    use super::*;
    use codeapp_types::{ContentBlock, Message, Role, ToolStatus, TranscriptKind};

    #[test]
    fn test_history_to_items_pairs_tool_use_and_result() {
        let agent = AgentId::main();
        let call_id = CallId::generate();

        let msgs = vec![
            (0, Arc::new(Message::user("run echo"))),
            (
                1,
                Arc::new(Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Text {
                            text: "Running echo...".into(),
                        },
                        ContentBlock::ToolUse {
                            id: call_id.clone(),
                            name: "bash".into(),
                            input: serde_json::json!({"command": "echo hi"}),
                        },
                    ],
                )),
            ),
            (
                2,
                Arc::new(Message::tool_result(call_id.clone(), "hi\n", false)),
            ),
            (
                3,
                Arc::new({
                    let mut m = Message::user("hidden debug");
                    m.meta.hidden = true;
                    m
                }),
            ),
        ];

        let items = history_to_items(&agent, &msgs, 10);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].seq, 10);
        assert!(matches!(&items[0].kind, TranscriptKind::User { text } if text == "run echo"));

        assert_eq!(items[1].seq, 11);
        assert!(
            matches!(&items[1].kind, TranscriptKind::Assistant { text, complete } if text == "Running echo..." && *complete)
        );

        assert_eq!(items[2].seq, 12);
        match &items[2].kind {
            TranscriptKind::ToolCall {
                call_id: cid,
                name,
                status,
                preview,
                ..
            } => {
                assert_eq!(cid, &call_id);
                assert_eq!(name, "bash");
                assert_eq!(status, &ToolStatus::Ok);
                assert_eq!(preview, "hi\n");
            }
            other => panic!("expected ToolCall, got {:?}", other),
        }
    }
}
