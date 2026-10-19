//! Event sink (seq + broadcast), transcript items and delta coalescing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use pacode_types::{AgentId, CallId, Event, Message, TranscriptItem};
use tokio::sync::broadcast;

/// Per-session event fan-out with a monotonic seq.
///
/// `emit` takes the next seq and sends under one lock, so receivers see seqs in
/// order even when many agents emit at once (a bare `fetch_add` followed by `send`
/// let seq 244 overtake 243, which a client reads as a gap).
pub struct EventSink {
    seq: AtomicU64,
    /// Serialises "take a seq" + "send": held for two cheap operations, never across
    /// an await. Owner: this sink.
    emit_lock: std::sync::Mutex<()>,
    tx: broadcast::Sender<(u64, Event)>,
}

impl EventSink {
    /// Capacity 1024: a lagging client re-syncs with `GetSnapshot`.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            seq: AtomicU64::new(0),
            emit_lock: std::sync::Mutex::new(()),
            tx,
        }
    }

    pub fn emit(&self, event: Event) -> u64 {
        let _order = self.emit_lock.lock().unwrap_or_else(|p| p.into_inner());
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

    /// Live subscribers; `0` means `emit` has nobody to deliver to.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
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
    /// Only `Running` tool calls; removed on any terminal status (cap = calls in flight,
    /// owner = `upsert`).
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

    /// Seq of the item carrying `question`, when it is still in the tail. A
    /// question answered after the tail rolled over simply has nothing to update.
    pub fn find_question_seq(&self, id: &pacode_types::QuestionId) -> Option<u64> {
        self.tail.iter().rev().find_map(|item| match &item.kind {
            pacode_types::TranscriptKind::Question { question, .. } if &question.id == id => {
                Some(item.seq)
            }
            _ => None,
        })
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
            pacode_types::TranscriptKind::Assistant { complete, .. } => {
                if !*complete {
                    self.live_assistant = Some(item.seq);
                } else if self.live_assistant == Some(item.seq) {
                    self.live_assistant = None;
                }
            }
            pacode_types::TranscriptKind::Reasoning { complete, .. } => {
                if !*complete {
                    self.live_reasoning = Some(item.seq);
                } else if self.live_reasoning == Some(item.seq) {
                    self.live_reasoning = None;
                }
            }
            pacode_types::TranscriptKind::ToolCall {
                call_id, status, ..
            } => match status {
                pacode_types::ToolStatus::Running => {
                    self.tool_items.insert(call_id.clone(), item);
                }
                pacode_types::ToolStatus::Ok
                | pacode_types::ToolStatus::Error
                | pacode_types::ToolStatus::Backgrounded
                | pacode_types::ToolStatus::Denied => {
                    self.tool_items.remove(call_id);
                }
            },
            pacode_types::TranscriptKind::User { .. }
            | pacode_types::TranscriptKind::Notice { .. }
            | pacode_types::TranscriptKind::Permission(_)
            | pacode_types::TranscriptKind::Question { .. }
            | pacode_types::TranscriptKind::Images { .. }
            | pacode_types::TranscriptKind::BashCommand { .. } => {}
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
    use pacode_types::{ContentBlock, Role, ToolStatus, TranscriptKind};

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
                let images = msg.image_refs();
                if !images.is_empty() {
                    items.push(TranscriptItem {
                        seq: cur_seq,
                        agent: agent.clone(),
                        ts_ms: msg.meta.timestamp_ms,
                        kind: TranscriptKind::Images { images },
                    });
                    cur_seq += 1;
                }
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
                            let intent = pacode_tools::intent_of(input);
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
                        ContentBlock::ToolResult { .. } | ContentBlock::Image { .. } => {}
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
                        *preview = pacode_types::truncate_head_tail(content, 256);
                    }
                }
                // Images a tool returned render right below its call row.
                let images = msg.image_refs();
                if !images.is_empty() {
                    items.push(TranscriptItem {
                        seq: cur_seq,
                        agent: agent.clone(),
                        ts_ms: msg.meta.timestamp_ms,
                        kind: TranscriptKind::Images { images },
                    });
                    cur_seq += 1;
                }
            }
            Role::System => {}
        }
    }

    items
}

/// Coalesces text deltas: flush when ≥ `max_bytes` or `max_age` since the first
/// buffered byte. The turn loop calls `push` on each delta and `take_if_due`/`take`
/// around its stream polling, arming a timer for `deadline()` only while data is buffered.
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

    /// When the buffered data becomes due; `None` while nothing is buffered.
    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|since| since + self.max_age)
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
    use pacode_types::{ContentBlock, Message, Role, ToolStatus, TranscriptKind};

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
    #[test]
    fn test_history_to_items_emits_images_after_user_and_tool_rows() {
        let agent = AgentId::main();
        let call_id = CallId::generate();
        let image = pacode_types::ImageRef {
            hash: "a".repeat(64),
            mime: "image/png".into(),
            width: Some(4),
            height: Some(4),
            name: Some("a.png".into()),
        };
        let msgs = vec![
            (
                0,
                Arc::new(Message::user_with_images(
                    "look",
                    std::slice::from_ref(&image),
                )),
            ),
            (
                1,
                Arc::new(Message::new(
                    Role::Assistant,
                    vec![ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "b.png"}),
                    }],
                )),
            ),
            (
                2,
                Arc::new(Message::tool_result_with_images(
                    call_id,
                    "image file",
                    false,
                    std::slice::from_ref(&image),
                )),
            ),
        ];
        let items = history_to_items(&agent, &msgs, 0);
        assert_eq!(items.len(), 4);
        assert!(matches!(&items[0].kind, TranscriptKind::User { text } if text == "look"));
        assert!(
            matches!(&items[1].kind, TranscriptKind::Images { images } if images == &vec![image.clone()])
        );
        assert!(matches!(&items[2].kind, TranscriptKind::ToolCall { .. }));
        assert!(matches!(&items[3].kind, TranscriptKind::Images { .. }));
        assert_eq!(
            items.iter().map(|i| i.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod transcript_tests;
