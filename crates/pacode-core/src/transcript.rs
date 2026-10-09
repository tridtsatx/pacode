//! Event sink (seq + broadcast), transcript items and delta coalescing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
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
    publish: Arc<Mutex<()>>,
}

impl EventSink {
    /// Capacity 1024: a lagging client re-syncs with `GetSnapshot`.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            seq: Arc::new(AtomicU64::new(0)),
            tx,
            publish: Arc::new(Mutex::new(())),
        }
    }

    pub fn emit(&self, event: Event) -> u64 {
        let _publish = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

const TAIL_BYTES_CAP: usize = 4 * 1024 * 1024;
const TOOL_ITEMS_CAP: usize = 200;
const TOOL_ITEMS_BYTES_CAP: usize = 4 * 1024 * 1024;

fn sum_bytes(values: impl IntoIterator<Item = usize>) -> usize {
    values.into_iter().fold(0usize, usize::saturating_add)
}

fn option_string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn transcript_item_bytes(item: &TranscriptItem) -> usize {
    let base = sum_bytes([
        std::mem::size_of::<TranscriptItem>(),
        item.agent.as_str().len(),
    ]);
    let kind = match &item.kind {
        pacode_types::TranscriptKind::User { text }
        | pacode_types::TranscriptKind::Assistant { text, .. }
        | pacode_types::TranscriptKind::Reasoning { text, .. } => text.capacity(),
        pacode_types::TranscriptKind::ToolCall {
            call_id,
            name,
            title,
            intent,
            preview,
            task,
            ..
        } => sum_bytes([
            call_id.as_str().len(),
            name.capacity(),
            title.capacity(),
            option_string_bytes(intent),
            preview.capacity(),
            task.as_ref().map_or(0, |id| id.as_str().len()),
        ]),
        pacode_types::TranscriptKind::Notice { text, .. } => text.capacity(),
        pacode_types::TranscriptKind::Permission(request) => sum_bytes([
            request.id.as_str().len(),
            request.agent.as_str().len(),
            request.agent_name.capacity(),
            request.call_id.as_str().len(),
            request.tool.capacity(),
            request.title.capacity(),
            request.detail.capacity(),
        ]),
        pacode_types::TranscriptKind::Question { question, answer } => {
            let options = sum_bytes(question.options.iter().map(|option| {
                sum_bytes([
                    std::mem::size_of::<pacode_types::QuestionOption>(),
                    option.label.capacity(),
                    option.description.capacity(),
                ])
            }));
            let answer = answer.as_ref().map_or(0, |answer| {
                sum_bytes([
                    answer
                        .selected
                        .capacity()
                        .saturating_mul(std::mem::size_of::<usize>()),
                    answer.free_text.as_ref().map_or(0, String::capacity),
                ])
            });
            sum_bytes([
                question.id.as_str().len(),
                question.agent.as_str().len(),
                question.agent_name.capacity(),
                question.call_id.as_str().len(),
                question.header.capacity(),
                question.question.capacity(),
                question
                    .options
                    .capacity()
                    .saturating_mul(std::mem::size_of::<pacode_types::QuestionOption>()),
                options,
                answer,
            ])
        }
        pacode_types::TranscriptKind::BashCommand {
            command, output, ..
        } => sum_bytes([command.capacity(), output.capacity()]),
    };
    sum_bytes([base, kind])
}

fn tool_item_entry_bytes(call_id: &CallId, item: &TranscriptItem) -> usize {
    sum_bytes([
        transcript_item_bytes(item),
        std::mem::size_of::<CallId>().saturating_mul(2),
        call_id.as_str().len().saturating_mul(2),
    ])
}

/// Per-agent transcript bookkeeping: item seqs, the live assistant/reasoning items,
/// tool-call items by call id, and a bounded tail for snapshots.
pub struct TranscriptState {
    pub next_item_seq: u64,
    pub live_assistant: Option<u64>,
    pub live_reasoning: Option<u64>,
    pending_item_seqs: HashSet<u64>,
    pub tool_items: HashMap<CallId, TranscriptItem>,
    tool_item_order: VecDeque<CallId>,
    tool_items_bytes: usize,
    /// Bounded (`session.history_page × 2`) tail of items for snapshots/history.
    pub tail: VecDeque<TranscriptItem>,
    pub tail_cap: usize,
    tail_bytes: usize,
}

impl TranscriptState {
    pub fn new(tail_cap: usize) -> Self {
        Self {
            next_item_seq: 0,
            live_assistant: None,
            live_reasoning: None,
            pending_item_seqs: HashSet::new(),
            tool_items: HashMap::new(),
            tool_item_order: VecDeque::new(),
            tool_items_bytes: 0,
            tail: VecDeque::new(),
            tail_cap,
            tail_bytes: 0,
        }
    }

    /// Lift the counter above the band reserved for store history, so this
    /// transcript's items never collide with a page read back from the store.
    pub fn reserve_history_band(&mut self) {
        self.next_item_seq = self.next_item_seq.max(HISTORY_ITEM_BASE);
    }

    /// Forget every item (`/clear`) but keep the counter: a late update for an
    /// item from before the clear then finds no row and is not new, so it
    /// cannot bring that item back.
    pub fn clear(&mut self) {
        *self = Self {
            next_item_seq: self.next_item_seq,
            ..Self::new(self.tail_cap)
        };
    }

    pub fn next_seq(&mut self) -> u64 {
        let seq = self.next_item_seq;
        self.next_item_seq = self.next_item_seq.saturating_add(1);
        self.pending_item_seqs.insert(seq);
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

    /// Seq of the *only* running tool call in the tail, or `None` when there is
    /// none or more than one.
    ///
    /// Live output arrives from the exec layer addressed to a task, but a
    /// running call's cell does not know its task id yet — that arrives with the
    /// result, once the process has already finished. So the row is found by the
    /// one running call the agent has, and **only when there is exactly one**:
    /// with two calls in flight, guessing would put a build's output under
    /// someone else's command, and dropped output is a far smaller failure than
    /// output in the wrong row.
    pub fn sole_running_tool_call_seq(&self) -> Option<u64> {
        let mut found = None;
        for item in self.tail.iter().rev() {
            if let pacode_types::TranscriptKind::ToolCall {
                status: pacode_types::ToolStatus::Running,
                ..
            } = item.kind
            {
                if found.is_some() {
                    return None;
                }
                found = Some(item.seq);
            }
        }
        found
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

    /// Resident bytes of the tail. Reported by the runtime stats so a session's
    /// memory cost is observable rather than inferred from a row count.
    pub fn tail_bytes(&self) -> u64 {
        self.tail_bytes as u64
    }

    /// Insert or replace (same seq) in the tail, evicting the oldest past the cap.
    pub fn upsert(&mut self, item: TranscriptItem) {
        let is_new = self.pending_item_seqs.remove(&item.seq) || item.seq >= self.next_item_seq;
        self.next_item_seq = self.next_item_seq.max(item.seq.saturating_add(1));
        let item_bytes = transcript_item_bytes(&item);
        let existing_index = self
            .tail
            .iter()
            .position(|existing| existing.seq == item.seq);
        let tail_accepted = if let Some(index) = existing_index {
            self.tail_bytes = self
                .tail_bytes
                .saturating_sub(transcript_item_bytes(&self.tail[index]));
            self.tail[index] = item.clone();
            self.tail_bytes = self.tail_bytes.saturating_add(item_bytes);
            true
        } else if is_new {
            let index = self
                .tail
                .iter()
                .position(|existing| existing.seq > item.seq)
                .unwrap_or(self.tail.len());
            self.tail.insert(index, item.clone());
            self.tail_bytes = self.tail_bytes.saturating_add(item_bytes);
            true
        } else {
            false
        };
        self.evict_tail();
        self.update_live_items(&item, tail_accepted);
        self.update_tool_item(&item, tail_accepted);
    }

    fn evict_tail(&mut self) {
        while self.tail.len() > self.tail_cap || self.tail_bytes > TAIL_BYTES_CAP {
            let Some(item) = self.tail.pop_front() else {
                break;
            };
            self.tail_bytes = self.tail_bytes.saturating_sub(transcript_item_bytes(&item));
        }
    }

    fn update_live_items(&mut self, item: &TranscriptItem, tail_accepted: bool) {
        match &item.kind {
            pacode_types::TranscriptKind::Assistant { complete, .. } => {
                if !*complete && tail_accepted {
                    self.live_assistant = Some(item.seq);
                } else if *complete && self.live_assistant == Some(item.seq) {
                    self.live_assistant = None;
                }
            }
            pacode_types::TranscriptKind::Reasoning { complete, .. } => {
                if !*complete && tail_accepted {
                    self.live_reasoning = Some(item.seq);
                } else if *complete && self.live_reasoning == Some(item.seq) {
                    self.live_reasoning = None;
                }
            }
            _ => {}
        }
    }

    fn update_tool_item(&mut self, item: &TranscriptItem, tail_accepted: bool) {
        let pacode_types::TranscriptKind::ToolCall { call_id, .. } = &item.kind else {
            return;
        };
        if !tail_accepted
            && self
                .tool_items
                .get(call_id)
                .is_none_or(|existing| existing.seq != item.seq)
        {
            return;
        }
        let previous = self.tool_items.insert(call_id.clone(), item.clone());
        if previous
            .as_ref()
            .is_none_or(|previous| previous.seq != item.seq)
            || !self.tool_item_order.contains(call_id)
        {
            self.tool_item_order.retain(|id| id != call_id);
            let index = self
                .tool_item_order
                .iter()
                .position(|id| {
                    self.tool_items
                        .get(id)
                        .is_some_and(|existing| existing.seq > item.seq)
                })
                .unwrap_or(self.tool_item_order.len());
            self.tool_item_order.insert(index, call_id.clone());
        }
        self.tool_items_bytes = sum_bytes(
            self.tool_items
                .iter()
                .map(|(call_id, item)| tool_item_entry_bytes(call_id, item)),
        );
        self.evict_tool_items();
    }

    fn evict_tool_items(&mut self) {
        while self.tool_items.len() > TOOL_ITEMS_CAP || self.tool_items_bytes > TOOL_ITEMS_BYTES_CAP
        {
            let Some(index) = self.tool_item_order.iter().position(|id| {
                self.tool_items.get(id).is_some_and(|item| {
                    !matches!(
                        &item.kind,
                        pacode_types::TranscriptKind::ToolCall {
                            status: pacode_types::ToolStatus::Running,
                            ..
                        }
                    )
                })
            }) else {
                break;
            };
            let Some(call_id) = self.tool_item_order.remove(index) else {
                break;
            };
            if let Some(item) = self.tool_items.remove(&call_id) {
                self.tool_items_bytes = self
                    .tool_items_bytes
                    .saturating_sub(tool_item_entry_bytes(&call_id, &item));
            }
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

/// First item seq reserved for the in-memory tail.
///
/// Item seqs are age-ordered per agent: a history page read out of the store is
/// always older than the tail, so its items must number *below* every tail
/// item. The tail counts up from wherever it starts, so a resumed session seeds
/// its counter here and store pages count down from here. The gap is
/// `HISTORY_ITEM_BASE` items wide, so it cannot be exhausted: at 500 items per
/// page that is over eight million pages of scrollback.
pub const HISTORY_ITEM_BASE: u64 = 1 << 32;

/// One projected transcript item together with the persisted message it came
/// from.
///
/// Item seqs and message seqs are different domains and neither is a function
/// of the other: one message expands into several items (an assistant turn
/// carries reasoning, text and a tool call), and message seqs have gaps
/// (cancelled and failed admissions never land). Handing a message seq to
/// something that expects an item seq — or numbering items by counting up from
/// a message seq — silently walks the cursor into the wrong window, so the
/// projection hands the mapping back and lets the caller number items from its
/// own authority.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryItem {
    /// `seq` of the message row this item was projected from. For a `ToolCall`
    /// that is the `tool_use` message, even when a later `tool_result` message
    /// is what completed it.
    pub message_seq: u64,
    /// `seq` is left at 0: the caller assigns it (see `HistoryItem`).
    pub item: TranscriptItem,
}

/// Convert persisted history into transcript items (used for resume and
/// `GetHistory` beyond the in-memory tail). Hidden messages are skipped; tool_use and
/// tool_result pairs become one `ToolCall` item with the final status.
pub fn history_to_items(agent: &AgentId, messages: &[(u64, Arc<Message>)]) -> Vec<HistoryItem> {
    use pacode_types::{ContentBlock, Role, ToolStatus, TranscriptKind};

    let mut items: Vec<HistoryItem> = Vec::new();
    let mut tool_indices: HashMap<CallId, usize> = HashMap::new();

    for (msg_seq, msg) in messages {
        if msg.meta.hidden {
            continue;
        }
        let push = |items: &mut Vec<HistoryItem>, kind| {
            items.push(HistoryItem {
                message_seq: *msg_seq,
                item: TranscriptItem {
                    seq: 0,
                    agent: agent.clone(),
                    ts_ms: msg.meta.timestamp_ms,
                    kind,
                },
            });
        };

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
                push(&mut items, TranscriptKind::User { text });
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Reasoning { text, .. } => {
                            if !text.is_empty() {
                                push(
                                    &mut items,
                                    TranscriptKind::Reasoning {
                                        text: text.clone(),
                                        complete: true,
                                    },
                                );
                            }
                        }
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                push(
                                    &mut items,
                                    TranscriptKind::Assistant {
                                        text: text.clone(),
                                        complete: true,
                                    },
                                );
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let title = tool_title(name, input, crate::turn::tools::TITLE_KEYS);
                            let intent = pacode_tools::intent_of(input);
                            let item_idx = items.len();
                            push(
                                &mut items,
                                TranscriptKind::ToolCall {
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
                            );
                            tool_indices.insert(id.clone(), item_idx);
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
                        } = items[idx].item.kind
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
        if self.buf.is_empty() && !text.is_empty() && text.len() >= self.max_bytes {
            self.since = None;
            return Some(text.to_owned());
        }
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
    use pacode_types::{ContentBlock, Message, Role, ToastLevel, ToolStatus, TranscriptKind};
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn the_running_tool_row_is_found_only_when_there_is_exactly_one() {
        // Live output arrives from the exec layer addressed to a task, but a
        // running call's cell does not know its task id yet. So the row is the
        // agent's only running call — and with two in flight the answer has to
        // be "none", because guessing would file a build's output under
        // someone else's command.
        let mut transcript = TranscriptState::new(64);
        assert_eq!(transcript.sole_running_tool_call_seq(), None);

        transcript.upsert(test_tool_item(1, &CallId::new("a"), ToolStatus::Running));
        assert_eq!(transcript.sole_running_tool_call_seq(), Some(1));

        transcript.upsert(test_tool_item(2, &CallId::new("b"), ToolStatus::Running));
        assert_eq!(
            transcript.sole_running_tool_call_seq(),
            None,
            "two calls in flight: no row is safe to attribute output to"
        );

        // One of them finishes: the survivor is unambiguous again.
        let mut done = test_tool_item(2, &CallId::new("b"), ToolStatus::Ok);
        done.seq = 2;
        transcript.upsert(done);
        assert_eq!(transcript.sole_running_tool_call_seq(), Some(1));
    }

    fn test_tool_item(seq: u64, call_id: &CallId, status: ToolStatus) -> TranscriptItem {
        TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq,
            kind: TranscriptKind::ToolCall {
                call_id: call_id.clone(),
                name: "bash".into(),
                title: "command".into(),
                intent: None,
                status,
                preview: String::new(),
                diff: None,
                duration_ms: None,
                task: None,
            },
        }
    }

    #[test]
    fn test_event_sink_clones_publish_in_sequence_order() {
        const EMITTERS: usize = 8;
        const EVENTS_PER_EMITTER: usize = 128;

        let sink = EventSink::new();
        let clone = sink.clone();
        assert!(Arc::ptr_eq(&sink.publish, &clone.publish));
        drop(clone);

        let mut receiver = sink.subscribe();
        let start = Arc::new(Barrier::new(EMITTERS));
        let handles = (0..EMITTERS)
            .map(|emitter| {
                let sink = sink.clone();
                let start = start.clone();
                thread::spawn(move || {
                    start.wait();
                    (0..EVENTS_PER_EMITTER)
                        .map(|index| {
                            let title = format!("{emitter}:{index}");
                            let seq = sink.emit(Event::Toast {
                                level: ToastLevel::Info,
                                title: title.clone(),
                                detail: None,
                            });
                            (title, seq)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let expected = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<HashMap<_, _>>();
        assert_eq!(expected.len(), EMITTERS * EVENTS_PER_EMITTER);

        let received = (0..EMITTERS * EVENTS_PER_EMITTER)
            .map(|_| receiver.try_recv().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            received.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            (1..=u64::try_from(expected.len()).unwrap()).collect::<Vec<_>>()
        );
        for (seq, event) in received {
            let Event::Toast { title, .. } = event else {
                panic!("expected Toast, got {event:?}");
            };
            assert_eq!(expected.get(&title), Some(&seq));
        }
    }

    #[test]
    fn event_sink_recovers_from_poisoned_publish_lock() {
        let sink = EventSink::new();
        let poisoner = sink.clone();
        thread::spawn(move || {
            let _guard = poisoner.publish.lock().unwrap();
            panic!("poison");
        })
        .join()
        .unwrap_err();

        assert_eq!(
            sink.emit(Event::Toast {
                level: ToastLevel::Info,
                title: "recovered".into(),
                detail: None,
            }),
            1
        );
    }

    #[test]
    fn allocated_item_seqs_can_be_upserted_out_of_order() {
        let mut state = TranscriptState::new(10);
        let first = state.next_seq();
        let second = state.next_seq();
        for seq in [second, first] {
            state.upsert(TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: format!("item-{seq}"),
                },
            });
        }
        assert_eq!(
            state.tail.iter().map(|item| item.seq).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(state.pending_item_seqs.is_empty());
    }

    #[test]
    fn oversized_delta_is_not_rebuffered() {
        let mut coalescer = DeltaCoalescer::new();
        let delta = "x".repeat(1024 * 1024);
        let emitted = coalescer
            .push(&delta, Instant::now())
            .expect("oversized delta");
        assert_eq!(emitted.len(), delta.len());
        assert_eq!(emitted.as_bytes(), delta.as_bytes());
        assert!(coalescer.take().is_none());
    }

    #[test]
    fn tool_item_cache_is_bounded() {
        let mut state = TranscriptState::new(1_000);
        let ids = (0..TOOL_ITEMS_CAP + 10)
            .map(|index| CallId::new(format!("call-{index}")))
            .collect::<Vec<_>>();
        for (index, call_id) in ids.iter().enumerate() {
            state.upsert(test_tool_item(
                u64::try_from(index).unwrap(),
                call_id,
                ToolStatus::Ok,
            ));
        }
        assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP);
        assert!(!state.tool_items.contains_key(&ids[0]));
        assert!(state.tool_items.contains_key(ids.last().unwrap()));
        assert!(state.tool_items_bytes <= TOOL_ITEMS_BYTES_CAP);
    }

    #[test]
    fn transcript_tail_is_bounded_by_bytes() {
        let mut state = TranscriptState::new(10);
        for seq in 0..2 {
            state.upsert(TranscriptItem {
                seq,
                agent: AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: "x".repeat(TAIL_BYTES_CAP / 2 + 1),
                },
            });
        }
        assert_eq!(state.tail.len(), 1);
        assert_eq!(state.tail[0].seq, 1);
        assert!(state.tail_bytes <= TAIL_BYTES_CAP);
    }

    #[test]
    fn tool_item_cache_is_bounded_by_bytes() {
        let mut state = TranscriptState::new(10);
        let ids = [CallId::new("first"), CallId::new("second")];
        for (seq, call_id) in ids.iter().enumerate() {
            let mut item = test_tool_item(u64::try_from(seq).unwrap(), call_id, ToolStatus::Ok);
            let TranscriptKind::ToolCall { preview, .. } = &mut item.kind else {
                unreachable!();
            };
            *preview = "x".repeat(TOOL_ITEMS_BYTES_CAP / 2 + 1);
            state.upsert(item);
        }
        assert_eq!(state.tool_items.len(), 1);
        assert!(!state.tool_items.contains_key(&ids[0]));
        assert!(state.tool_items.contains_key(&ids[1]));
        assert!(state.tool_items_bytes <= TOOL_ITEMS_BYTES_CAP);
    }

    #[test]
    fn running_tool_items_are_not_evicted() {
        let mut state = TranscriptState::new(TOOL_ITEMS_CAP);
        let ids = (0..=TOOL_ITEMS_CAP)
            .map(|index| CallId::new(format!("running-{index}")))
            .collect::<Vec<_>>();
        for (seq, call_id) in ids.iter().enumerate() {
            state.upsert(test_tool_item(
                u64::try_from(seq).unwrap(),
                call_id,
                ToolStatus::Running,
            ));
        }
        assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP + 1);
        assert!(ids.iter().all(|id| state.tool_items.contains_key(id)));
        assert!(state.tool_items.values().all(|item| {
            matches!(
                &item.kind,
                TranscriptKind::ToolCall {
                    status: ToolStatus::Running,
                    ..
                }
            )
        }));
    }

    #[test]
    fn late_terminal_update_uses_retained_running_state() {
        let mut state = TranscriptState::new(1);
        let first = CallId::new("first");
        let second = CallId::new("second");
        state.upsert(test_tool_item(0, &first, ToolStatus::Running));
        state.upsert(test_tool_item(1, &second, ToolStatus::Running));
        assert!(!state.tail.iter().any(|item| item.seq == 0));

        let mut terminal = state.tool_item(&first).cloned().unwrap();
        let TranscriptKind::ToolCall {
            status, preview, ..
        } = &mut terminal.kind
        else {
            unreachable!();
        };
        *status = ToolStatus::Ok;
        *preview = "done".into();
        state.upsert(terminal);

        assert!(matches!(
            state.tool_item(&first).map(|item| &item.kind),
            Some(TranscriptKind::ToolCall {
                status: ToolStatus::Ok,
                ..
            })
        ));
        assert!(!state.tail.iter().any(|item| item.seq == 0));
    }

    #[test]
    fn late_terminal_update_does_not_restore_fully_evicted_item() {
        let mut state = TranscriptState::new(1);
        let ids = (0..=TOOL_ITEMS_CAP)
            .map(|index| CallId::new(format!("terminal-{index}")))
            .collect::<Vec<_>>();
        for (seq, call_id) in ids.iter().enumerate() {
            state.upsert(test_tool_item(
                u64::try_from(seq).unwrap(),
                call_id,
                ToolStatus::Ok,
            ));
        }
        assert!(!state.tool_items.contains_key(&ids[0]));

        state.upsert(test_tool_item(0, &ids[0], ToolStatus::Error));

        assert!(!state.tool_items.contains_key(&ids[0]));
        assert_eq!(state.tool_items.len(), TOOL_ITEMS_CAP);
        assert_eq!(state.tail.len(), 1);
        assert_eq!(state.tail[0].seq, u64::try_from(TOOL_ITEMS_CAP).unwrap());
    }

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

        let items = history_to_items(&agent, &msgs);
        assert_eq!(items.len(), 3);

        // Each item reports the message it was projected from, and the item seq
        // is left for the caller: item and message seqs are separate domains.
        assert_eq!(
            items.iter().map(|i| i.message_seq).collect::<Vec<_>>(),
            vec![0, 1, 1]
        );
        assert!(
            items.iter().all(|i| i.item.seq == 0),
            "the projection must not invent item seqs"
        );

        assert!(matches!(&items[0].item.kind, TranscriptKind::User { text } if text == "run echo"));

        assert!(
            matches!(&items[1].item.kind, TranscriptKind::Assistant { text, complete } if text == "Running echo..." && *complete)
        );

        // The tool item is tagged with its `tool_use` message (seq 1) even
        // though the message at seq 2 is what completed it.
        match &items[2].item.kind {
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
