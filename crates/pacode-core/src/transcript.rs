//! Event sink (seq + broadcast), transcript items and delta coalescing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use pacode_types::{AgentId, CallId, Event, Message, TranscriptItem};
use tokio::sync::broadcast;

/// Per-session event fan-out with a monotonic seq.
///
/// `Clone` shares the seq counter and the channel — a `Session` clones the
/// `Core` daemon-wide sink to publish `Event::SessionStatus` into the same
/// seq space.
#[derive(Clone)]
pub struct EventSink {
    seq: Arc<AtomicU64>,
    tx: broadcast::Sender<(u64, Event)>,
}

impl EventSink {
    /// Capacity 1024: a lagging client re-syncs with `GetSnapshot`.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            seq: Arc::new(AtomicU64::new(0)),
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

    /// Seq of the item carrying the `Permission` cell for `id`, while it is
    /// still in the tail — the mirror of `find_question_seq`, used when a
    /// pending permission is cancelled without its waiter (interrupt/stop).
    pub fn find_permission_seq(&self, id: &pacode_types::PermissionId) -> Option<u64> {
        self.tail.iter().rev().find_map(|item| match &item.kind {
            pacode_types::TranscriptKind::Permission(req) if &req.id == id => Some(item.seq),
            _ => None,
        })
    }

    /// The latest `ToolCall` item for `call_id`, if any — the cell the stream
    /// created at `ToolCallStart` and dispatch updates in place.
    pub fn tool_item(&self, call_id: &CallId) -> Option<&TranscriptItem> {
        self.tool_items.get(call_id)
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
            pacode_types::TranscriptKind::ToolCall { call_id, .. } => {
                self.tool_items.insert(call_id.clone(), item);
            }
            pacode_types::TranscriptKind::User { .. }
            | pacode_types::TranscriptKind::Notice { .. }
            | pacode_types::TranscriptKind::Permission(_)
            | pacode_types::TranscriptKind::Question { .. }
            | pacode_types::TranscriptKind::BashCommand { .. } => {}
        }
    }
}

/// `Bash cargo build`, `Edit src/tui/sidebar.rs` — the title is the first
/// *meaningful* string field: preferred keys (`command`, `path`, `pattern`, …)
/// win over field order, so `write {content, path}` still titles by the path.
pub(crate) fn tool_title(name: &str, input: &serde_json::Value, preferred: &[&str]) -> String {
    let with_arg = |s: &str| {
        let truncated: String = s.trim().chars().take(60).collect();
        format!("{name} {truncated}")
    };
    if let Some(obj) = input.as_object() {
        for key in preferred {
            if let Some(s) = obj.get(*key).and_then(serde_json::Value::as_str)
                && !s.trim().is_empty()
            {
                return with_arg(s);
            }
        }
        for (k, v) in obj {
            if k != "intent"
                && k != "accept_large_output"
                && let Some(s) = v.as_str()
                && !s.trim().is_empty()
            {
                return with_arg(s);
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
                let mut text = msg.text();
                // Attached images get a placeholder line so a reloaded session
                // still shows what the prompt carried.
                for block in &msg.content {
                    if let ContentBlock::Image { source } = block {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&source.placeholder());
                    }
                }
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
                            let title = tool_title(name, input, crate::turn::tools::TITLE_KEYS);
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
                        *preview = pacode_types::truncate_head_tail(
                            content,
                            pacode_types::PREVIEW_CAP_CHARS,
                        );
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
    fn tool_title_prefers_meaningful_keys_over_field_order() {
        let keys = crate::turn::tools::TITLE_KEYS;
        // `write` sends `content` first in JSON order — the title must still
        // come from `path`, not the file body.
        let input = serde_json::json!({
            "content": "#!/usr/bin/env python3\nprint('x')\n",
            "path": "tools/gen.py",
        });
        assert_eq!(tool_title("write", &input, keys), "write tools/gen.py");

        let input = serde_json::json!({"command": "cargo test", "timeout_secs": 30});
        assert_eq!(tool_title("bash", &input, keys), "bash cargo test");

        let input = serde_json::json!({"pattern": "TODO", "path": "src/"});
        assert_eq!(tool_title("grep", &input, keys), "grep TODO");
    }

    #[test]
    fn tool_title_falls_back_and_skips_meta_keys() {
        let keys = crate::turn::tools::TITLE_KEYS;
        // No preferred key: first non-empty string wins; `intent` and
        // `accept_large_output` never do.
        let input = serde_json::json!({
            "intent": "checking the build",
            "action": "status",
        });
        assert_eq!(tool_title("bg", &input, keys), "bg status");

        let input = serde_json::json!({"accept_large_output": true});
        assert_eq!(tool_title("read", &input, keys), "read");

        // A preferred key present but empty loses to the next preferred one.
        let input = serde_json::json!({"command": " ", "url": "https://x.dev"});
        assert_eq!(
            tool_title("webfetch", &input, keys),
            "webfetch https://x.dev"
        );
    }

    #[test]
    fn tool_title_truncates_long_args() {
        let keys = crate::turn::tools::TITLE_KEYS;
        let long = "x".repeat(200);
        let input = serde_json::json!({"command": long});
        let title = tool_title("bash", &input, keys);
        assert_eq!(title, format!("bash {}", "x".repeat(60)));
    }
}
