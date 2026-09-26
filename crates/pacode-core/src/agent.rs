//! `Agent`: history, status and transcript bookkeeping for the main agent and each
//! subagent.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentInfo, AgentStatus, CallId, ContentBlock, Message, Role, TranscriptItem, TurnId,
    TurnStop, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::inject::InjectionQueue;
use crate::notice_lock::{DueOrigin, Taken};
use crate::transcript::TranscriptState;

/// Content of a synthesized `tool_result` answering a `tool_use` that never
/// produced one: the turn was interrupted or died mid-dispatch, or persisted
/// history was loaded with the pairing already broken. `is_error` is set so
/// adapters can mark it as a failed call.
pub(crate) const INTERRUPTED_TOOL_RESULT: &str = "[interrupted]";

pub(crate) struct MessageReservation {
    seq: u64,
    message: Arc<Message>,
    persisted: std::sync::atomic::AtomicBool,
    finalized: std::sync::atomic::AtomicBool,
    admission: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
pub(crate) struct TurnAdmission {
    pub(crate) message_id: String,
    pub(crate) turn: TurnId,
    pub(crate) stop: Option<TurnStop>,
    progressed: bool,
    cancelled: bool,
}

pub(crate) struct AdmissionFinish {
    pub(crate) message_id: String,
    pub(crate) terminal: bool,
}

#[derive(Default)]
struct TurnFenceState {
    current: Option<TurnId>,
    terminal: Option<TurnId>,
    terminal_error: Option<String>,
}

const MAX_CANCELLED_ADMISSIONS: usize = 1024;

#[derive(Default)]
struct CancelledAdmissions {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl CancelledAdmissions {
    fn insert(&mut self, message_id: &str) {
        if !self.ids.insert(message_id.to_owned()) {
            return;
        }
        self.order.push_back(message_id.to_owned());
        if self.order.len() > MAX_CANCELLED_ADMISSIONS
            && let Some(expired) = self.order.pop_front()
        {
            self.ids.remove(&expired);
        }
    }

    fn contains(&self, message_id: &str) -> bool {
        self.ids.contains(message_id)
    }
}

impl MessageReservation {
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    pub(crate) fn message(&self) -> &Arc<Message> {
        &self.message
    }

    pub(crate) fn persist(&self) {
        self.persisted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn set_admission(&self, admission: String) {
        *self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(admission);
    }

    pub(crate) fn admission(&self) -> Option<String> {
        self.admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn is_persisted(&self) -> bool {
        self.persisted.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn discard(&self) {
        self.finalized
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The model-visible history of one agent: a compaction summary (if any) plus the
/// messages after it. Messages are `Arc` so a forked subagent shares them.
#[derive(Clone, Default)]
pub struct History {
    pub messages: Vec<Arc<Message>>,
    pub(crate) message_seqs: Vec<u64>,
    pub(crate) free_seqs: BTreeSet<u64>,
    /// Seq of the next message (per session+agent, persisted with the message).
    pub next_seq: u64,
    pub summary: Option<String>,
    /// Seq up to which the summary replaces history.
    pub summary_upto: u64,
    /// Estimated tokens of `messages` (kept incrementally; refreshed by real usage).
    pub estimated_tokens: u32,
}

impl History {
    /// Append and return the assigned seq.
    pub fn push(&mut self, msg: Message) -> (u64, Arc<Message>) {
        let reservation = self.reserve(msg);
        let seq = reservation.seq();
        let message = reservation.message().clone();
        self.commit_reserved(&reservation);
        (seq, message)
    }

    pub(crate) fn reserve(&mut self, msg: Message) -> Arc<MessageReservation> {
        let seq = self.free_seqs.pop_first().unwrap_or_else(|| {
            let seq = self.next_seq;
            self.next_seq = self.next_seq.saturating_add(1);
            seq
        });
        Arc::new(MessageReservation {
            seq,
            message: Arc::new(msg),
            persisted: std::sync::atomic::AtomicBool::new(false),
            finalized: std::sync::atomic::AtomicBool::new(false),
            admission: Mutex::new(None),
        })
    }

    pub(crate) fn commit_reserved(&mut self, reservation: &MessageReservation) {
        if reservation
            .finalized
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let seq = reservation.seq;
        if self.message_seqs.last().is_none_or(|last| *last < seq) {
            self.estimated_tokens = self
                .estimated_tokens
                .saturating_add(reservation.message.estimate_tokens());
            self.message_seqs.push(seq);
            self.messages.push(reservation.message.clone());
            return;
        }
        let index = match self.message_seqs.binary_search(&seq) {
            Ok(index) => {
                let previous_tokens = self.messages[index].estimate_tokens();
                self.messages[index] = reservation.message.clone();
                self.estimated_tokens = self
                    .estimated_tokens
                    .saturating_sub(previous_tokens)
                    .saturating_add(reservation.message.estimate_tokens());
                return;
            }
            Err(index) => index,
        };
        self.estimated_tokens = self
            .estimated_tokens
            .saturating_add(reservation.message.estimate_tokens());
        self.message_seqs.insert(index, seq);
        self.messages.insert(index, reservation.message.clone());
    }

    pub(crate) fn rollback_reserved(&mut self, reservation: &MessageReservation) {
        if reservation
            .finalized
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        if reservation.is_persisted() {
            return;
        }
        let seq = reservation.seq;
        if self.message_seqs.binary_search(&seq).is_ok() {
            return;
        }
        self.free_seqs.insert(seq);
        while self.free_seqs.contains(&self.next_seq.saturating_sub(1)) && self.next_seq > 0 {
            self.free_seqs.remove(&self.next_seq.saturating_sub(1));
            self.next_seq -= 1;
        }
    }

    pub(crate) fn restore_row(&mut self, seq: u64, message: Arc<Message>) {
        self.free_seqs.remove(&seq);
        self.next_seq = self.next_seq.max(seq.saturating_add(1));
        if self.message_seqs.last().is_none_or(|last| *last < seq) {
            self.message_seqs.push(seq);
            self.messages.push(message.clone());
            self.estimated_tokens = self
                .estimated_tokens
                .saturating_add(message.estimate_tokens());
            return;
        }
        let index = match self.message_seqs.binary_search(&seq) {
            Ok(_) => return,
            Err(index) => index,
        };
        self.message_seqs.insert(index, seq);
        self.messages.insert(index, message.clone());
        self.estimated_tokens = self
            .estimated_tokens
            .saturating_add(message.estimate_tokens());
    }

    #[cfg(test)]
    pub(crate) fn restore_rows(&mut self, mut rows: Vec<(u64, Arc<Message>)>) {
        if rows.is_empty() {
            return;
        }
        rows.sort_unstable_by_key(|(seq, _)| *seq);
        rows.dedup_by(|left, right| left.0 == right.0);
        for (seq, _) in &rows {
            self.free_seqs.remove(seq);
            self.next_seq = self.next_seq.max(seq.saturating_add(1));
        }

        if self
            .message_seqs
            .last()
            .is_none_or(|last| rows[0].0 > *last)
        {
            self.message_seqs.reserve(rows.len());
            self.messages.reserve(rows.len());
            for (seq, message) in rows {
                self.message_seqs.push(seq);
                self.messages.push(message);
            }
            self.recompute_token_estimate();
            return;
        }

        let old_seqs = std::mem::take(&mut self.message_seqs);
        let old_messages = std::mem::take(&mut self.messages);
        let total = old_seqs.len().saturating_add(rows.len());
        let mut merged_seqs = Vec::with_capacity(total);
        let mut merged_messages = Vec::with_capacity(total);
        let old = old_seqs.into_iter().zip(old_messages);
        let mut restored = rows.into_iter().peekable();
        for (old_seq, old_message) in old {
            if let Some(&(restored_seq, _)) = restored.peek() {
                if old_seq < restored_seq {
                    merged_seqs.push(old_seq);
                    merged_messages.push(old_message);
                    continue;
                }
                if old_seq > restored_seq {
                    if let Some((seq, message)) = restored.next() {
                        merged_seqs.push(seq);
                        merged_messages.push(message);
                    }
                    continue;
                }
                drop(restored.next());
            }
            merged_seqs.push(old_seq);
            merged_messages.push(old_message);
        }
        for (seq, message) in restored {
            merged_seqs.push(seq);
            merged_messages.push(message);
        }
        self.message_seqs = merged_seqs;
        self.messages = merged_messages;
        self.recompute_token_estimate();
    }

    #[cfg(test)]
    fn recompute_token_estimate(&mut self) {
        self.estimated_tokens = self
            .summary
            .as_deref()
            .map(pacode_types::estimate_tokens)
            .unwrap_or(0)
            .saturating_add(
                self.messages
                    .iter()
                    .map(|message| message.estimate_tokens())
                    .sum::<u32>(),
            );
    }

    pub(crate) fn clear(&mut self) {
        self.messages = Vec::new();
        self.message_seqs = Vec::new();
        self.free_seqs = BTreeSet::new();
        self.summary = None;
        self.summary_upto = 0;
        self.estimated_tokens = 0;
    }

    pub fn rows(&self) -> Vec<(u64, Arc<Message>)> {
        self.message_seqs
            .iter()
            .copied()
            .zip(self.messages.iter().cloned())
            .collect()
    }

    /// Messages sent to the model: summary (as a user message) then the rest.
    ///
    /// Returns shared refs — a deep clone of every content block on every model
    /// step was a major allocation churn source on long histories. Providers
    /// only read the messages; the `Arc`s also keep a `CompletionRequest` clone
    /// cheap.
    ///
    /// The output passes through [`sanitize_tool_pairing_into`]: a turn that
    /// died mid-dispatch (or a persisted history already carrying the damage)
    /// would otherwise send `tool_use` blocks with no `tool_result` and every
    /// provider rejects the whole request (`400: tool_use ids found without
    /// tool_result`).
    pub fn for_model(&self) -> Vec<Arc<Message>> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        if let Some(summary) = &self.summary {
            out.push(Arc::new(
                Message::user(format!("[Previous conversation summary]\n{summary}"))
                    .with_kind(pacode_types::MessageKind::CompactionSummary),
            ));
        }
        sanitize_tool_pairing_into(&self.messages, &mut out);
        out
    }
}

/// Single funnel for "history → wire" pairing repair (spec §6.2): every
/// `ToolUse` block in an assistant message must be answered by a `ToolResult`
/// in the run of `Role::Tool` messages immediately following it (adapters merge
/// that run into the one user message the APIs require), and a `ToolResult`
/// anywhere else is itself invalid (`unexpected tool_use_id`).
///
/// A turn killed mid-dispatch — panic, task abort, daemon restart, a lost store
/// write — leaves persisted `tool_use`s unanswered and the provider then 400s
/// every subsequent turn until compaction. Repairing at this seam heals loaded
/// histories on the next request without rewriting the store. Returns the
/// number of synthesized blocks (for the caller's log line).
pub(crate) fn sanitize_tool_pairing(messages: &[Arc<Message>]) -> (Vec<Arc<Message>>, usize) {
    let mut out = Vec::with_capacity(messages.len());
    let repaired = sanitize_tool_pairing_into(messages, &mut out);
    (out, repaired)
}

fn sanitize_tool_pairing_into(messages: &[Arc<Message>], out: &mut Vec<Arc<Message>>) -> usize {
    let mut repaired = 0usize;
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role != Role::Assistant || !msg.has_tool_uses() {
            // A `Role::Tool` message outside an answering run can only carry
            // stray results — degrade them to text blocks (they still reach
            // the model but no longer trip the pairing check).
            out.push(sanitize_stray_tool_message(msg, &mut repaired));
            i += 1;
            continue;
        }

        out.push(msg.clone());
        i += 1;
        let uses: Vec<&CallId> = msg.tool_uses().map(|(id, _, _)| id).collect();
        // The answering run: consecutive `Role::Tool` messages. A non-`Tool`
        // message ends it — a later result is positionally stray and gets
        // degraded when the walk reaches it.
        let mut answered: Vec<CallId> = Vec::new();
        while i < messages.len() && messages[i].role == Role::Tool {
            out.push(sanitize_tool_message_answered(
                &messages[i],
                &uses,
                &mut answered,
                &mut repaired,
            ));
            i += 1;
        }
        for id in uses.iter().copied().filter(|id| !answered.contains(*id)) {
            out.push(Arc::new(Message::tool_result(
                id.clone(),
                INTERRUPTED_TOOL_RESULT,
                true,
            )));
            repaired += 1;
        }
    }
    repaired
}

/// `msg` (a `Role::Tool` message in an answering run): `ToolResult` blocks whose
/// `call_id` is still pending are kept and marked answered; everything else —
/// unknown or duplicate ids — is positionally stray and degrades to text.
fn sanitize_tool_message_answered(
    msg: &Arc<Message>,
    uses: &[&CallId],
    answered: &mut Vec<CallId>,
    repaired: &mut usize,
) -> Arc<Message> {
    let has_stray = msg.content.iter().any(|block| match block {
        ContentBlock::ToolResult { call_id, .. } => {
            !uses.contains(&call_id) || answered.contains(call_id)
        }
        ContentBlock::Text { .. }
        | ContentBlock::Reasoning { .. }
        | ContentBlock::ToolUse { .. }
        | ContentBlock::Image { .. } => false,
    });
    if !has_stray {
        for block in &msg.content {
            if let ContentBlock::ToolResult { call_id, .. } = block {
                answered.push(call_id.clone());
            }
        }
        return msg.clone();
    }

    *repaired += 1;
    let mut fixed = (**msg).clone();
    fixed.content = fixed
        .content
        .into_iter()
        .map(|block| match block {
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                if uses.contains(&&call_id) && !answered.contains(&call_id) {
                    answered.push(call_id.clone());
                    ContentBlock::ToolResult {
                        call_id,
                        content,
                        is_error,
                    }
                } else {
                    ContentBlock::Text {
                        text: format!("[unmatched tool result (call {call_id})]\n{content}"),
                    }
                }
            }
            other => other,
        })
        .collect();
    Arc::new(fixed)
}

/// `msg` with no pending `tool_use` run behind it: every `ToolResult` block is
/// stray and degrades to text. Cheap path returns the shared ref untouched.
fn sanitize_stray_tool_message(msg: &Arc<Message>, repaired: &mut usize) -> Arc<Message> {
    let has_results = msg
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }));
    if msg.role != Role::Tool || !has_results {
        return msg.clone();
    }
    *repaired += 1;
    let mut fixed = (**msg).clone();
    fixed.content = fixed
        .content
        .into_iter()
        .map(|block| match block {
            ContentBlock::ToolResult {
                call_id, content, ..
            } => ContentBlock::Text {
                text: format!("[unmatched tool result (call {call_id})]\n{content}"),
            },
            other => other,
        })
        .collect();
    Arc::new(fixed)
}

pub struct Agent {
    id: RwLock<Arc<AgentId>>,
    pub info: RwLock<AgentInfo>,
    pub history: Mutex<History>,
    pending_user_messages: Mutex<VecDeque<Arc<MessageReservation>>>,
    discarded_message_seqs: Mutex<HashSet<u64>>,
    admission_turn: Mutex<Option<TurnAdmission>>,
    cancelled_admissions: Mutex<CancelledAdmissions>,
    turn_fence: Mutex<TurnFenceState>,
    turn_usage: Mutex<Usage>,
    stream_usage: Mutex<Usage>,
    pub injections: InjectionQueue,
    /// The notice lock: the switch, and the backlog it parks instead of
    /// delivering. It sits next to `injections` because both drain sites
    /// (`request_turn` and `drain_injections`) read from one and put back into
    /// the other, and a take they later undo must not have spent a release.
    pub notice_lock: crate::notice_lock::NoticeLock,
    /// Tools this agent may call (main: all; subagent: subset without `agent`).
    pub tools: RwLock<ToolRegistry>,
    /// Cancellation token of the running turn, if any. Bookkeeping for the
    /// interrupt paths — `is_running` derives from `turn_lock` instead, so the
    /// whole post-block counts as "running" and a racing `start_turn` can
    /// never observe a turn-ended-but-still-locked gap.
    pub cancel: Mutex<Option<CancellationToken>>,
    /// Set by `stop_agent`/`interrupt_all`/`close_session` BEFORE the token is
    /// cancelled; the unwinding turn reads it to keep `Stopped` and skip the
    /// finished-report path. Survives status flaps mid-unwind. Cleared by
    /// `Session::continue_agent`.
    pub stop_requested: std::sync::atomic::AtomicBool,
    turn_generation: std::sync::atomic::AtomicU64,
    /// One turn at a time — and what `is_running` reads. `Arc` so
    /// `start_turn` can claim it with an owned guard and move it into the
    /// supervisor task, which holds it across the spawned turn's whole
    /// lifetime: run, token cleanup, `TurnEnded` emission, and the post-block
    /// (re-drive check + terminal landing).
    pub turn_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) message_write_lock: Arc<tokio::sync::Mutex<()>>,
    pub transcript: Mutex<TranscriptState>,
    /// Where the in-memory tail ends and the store begins, in **message** seqs.
    ///
    /// `Some(seq)` means the tail's oldest message is `seq` and everything older
    /// lives in the store, so a history page continues with `seq < this`.
    /// `None` means the tail already covers the whole conversation and there is
    /// nothing older to page. This is deliberately not derived from a
    /// `TranscriptItem::seq`: those are numbered by `TranscriptState` and have
    /// no arithmetic relation to message seqs (one message can become several
    /// items, and message seqs have gaps), so deriving one from the other walks
    /// the cursor into the wrong window.
    history_floor: Mutex<Option<u64>>,
    /// Next item seq to hand out for history read back from the store.
    ///
    /// Store pages are older than everything in the tail, so their items are
    /// numbered *downwards* from the tail's lowest seq. Numbering them upwards
    /// from a message seq (the old behaviour) let a page's items collide with
    /// the tail's, and the client keys cells and de-duplicates on that seq.
    older_item_seq: std::sync::atomic::AtomicU64,
    /// The prompt a subagent was spawned with (persisted).
    pub prompt: RwLock<Option<String>>,
    /// History snapshot as it stood before the current turn began (for turn detach).
    pub history_before_turn: Mutex<Option<History>>,
    /// Turn-scoped prompt cache (invalidation point: turn start).
    pub prompt_cache: Mutex<Option<crate::prompt::TurnPromptCache>>,
    /// When this agent was created, monotonic. `None` for the main agent and for
    /// a subagent resumed on reopen, where there is no spawn to measure from.
    /// The turn loop needs it to report time-to-first-token, which is the one
    /// launch stage that belongs to neither the spawn path nor the turn alone.
    spawned_at: Mutex<Option<std::time::Instant>>,
    /// Fusion attribution for the running or last turn: which model each half
    /// ran on and what it cost. `None` when fusion is off — which is every
    /// agent in a session with no `[ui] fusion` pair. Replaced wholesale at
    /// turn start, the same point that invalidates `prompt_cache`.
    pub fusion: Mutex<Option<crate::fusion::FusionTrace>>,
}

impl Agent {
    pub fn new(
        id: AgentId,
        info: AgentInfo,
        history: History,
        tools: ToolRegistry,
        history_page: u32,
        prompt: Option<String>,
    ) -> Self {
        Self {
            id: RwLock::new(Arc::new(id)),
            info: RwLock::new(info),
            history: Mutex::new(history),
            pending_user_messages: Mutex::new(VecDeque::new()),
            discarded_message_seqs: Mutex::new(HashSet::new()),
            admission_turn: Mutex::new(None),
            cancelled_admissions: Mutex::new(CancelledAdmissions::default()),
            turn_fence: Mutex::new(TurnFenceState::default()),
            turn_usage: Mutex::new(Usage::default()),
            stream_usage: Mutex::new(Usage::default()),
            injections: InjectionQueue::default(),
            notice_lock: crate::notice_lock::NoticeLock::new(),
            tools: RwLock::new(tools),
            cancel: Mutex::new(None),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            turn_generation: std::sync::atomic::AtomicU64::new(0),
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            message_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            transcript: Mutex::new(TranscriptState::new(history_page as usize * 2)),
            history_floor: Mutex::new(None),
            older_item_seq: std::sync::atomic::AtomicU64::new(crate::transcript::HISTORY_ITEM_BASE),
            prompt: RwLock::new(prompt),
            history_before_turn: Mutex::new(None),
            prompt_cache: Mutex::new(None),
            spawned_at: Mutex::new(None),
            fusion: Mutex::new(None),
        }
    }

    /// Mark this agent as freshly spawned, which starts the clock the turn loop
    /// reads to report time-to-first-token. Only the spawn path calls it: a
    /// resumed subagent has no spawn to measure, and measuring from its reopen
    /// would report resume cost as launch cost.
    pub(crate) fn mark_spawned_at(&self, at: std::time::Instant) {
        *self.spawned_at.lock().unwrap_or_else(|p| p.into_inner()) = Some(at);
    }

    /// How long ago this agent was spawned, if it was.
    pub(crate) fn since_spawn(&self) -> Option<std::time::Duration> {
        self.spawned_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map(|at| at.elapsed())
    }

    pub fn new_with_transcript(
        id: AgentId,
        info: AgentInfo,
        history: History,
        tools: ToolRegistry,
        transcript: TranscriptState,
        prompt: Option<String>,
    ) -> Self {
        Self {
            id: RwLock::new(Arc::new(id)),
            info: RwLock::new(info),
            history: Mutex::new(history),
            pending_user_messages: Mutex::new(VecDeque::new()),
            discarded_message_seqs: Mutex::new(HashSet::new()),
            admission_turn: Mutex::new(None),
            cancelled_admissions: Mutex::new(CancelledAdmissions::default()),
            turn_fence: Mutex::new(TurnFenceState::default()),
            turn_usage: Mutex::new(Usage::default()),
            stream_usage: Mutex::new(Usage::default()),
            injections: InjectionQueue::default(),
            notice_lock: crate::notice_lock::NoticeLock::new(),
            tools: RwLock::new(tools),
            cancel: Mutex::new(None),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            turn_generation: std::sync::atomic::AtomicU64::new(0),
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            message_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            transcript: Mutex::new(transcript),
            history_floor: Mutex::new(None),
            older_item_seq: std::sync::atomic::AtomicU64::new(crate::transcript::HISTORY_ITEM_BASE),
            prompt: RwLock::new(prompt),
            history_before_turn: Mutex::new(None),
            prompt_cache: Mutex::new(None),
            spawned_at: Mutex::new(None),
            fusion: Mutex::new(None),
        }
    }

    pub(crate) fn reserve_user_message(&self, msg: Message) -> Arc<MessageReservation> {
        let mut pending = self
            .pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reservation = history.reserve(msg);
        pending.push_back(reservation.clone());
        reservation
    }

    pub(crate) fn reserve_user_message_with_admission(
        &self,
        msg: Message,
        admission: String,
    ) -> Arc<MessageReservation> {
        let reservation = self.reserve_user_message(msg);
        reservation.set_admission(admission);
        reservation
    }

    pub(crate) fn rollback_user_message(&self, reservation: &Arc<MessageReservation>) {
        let mut pending = self
            .pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = pending
            .iter()
            .position(|candidate| Arc::ptr_eq(candidate, reservation))
        {
            pending.remove(index);
        }
        self.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rollback_reserved(reservation);
    }

    pub(crate) fn commit_pending_user_messages(&self) -> bool {
        let mut pending = self
            .pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ready = pending
            .iter()
            .take_while(|reservation| reservation.is_persisted())
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return false;
        }
        pending.drain(..ready.len());
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for reservation in &ready {
            history.commit_reserved(reservation);
        }
        true
    }

    pub(crate) fn discard_pending_user_messages(&self) -> Vec<String> {
        let mut pending_user_messages = self
            .pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut discarded_message_seqs = self
            .discarded_message_seqs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = pending_user_messages.drain(..).collect::<Vec<_>>();
        for reservation in &pending {
            reservation.discard();
            discarded_message_seqs.extend([reservation.seq()]);
        }
        pending
            .iter()
            .filter_map(|reservation| reservation.admission())
            .collect()
    }

    pub(crate) fn discarded_message_seqs(&self) -> HashSet<u64> {
        self.discarded_message_seqs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Everything queued or parked for a turn that is not coming. A stopped or
    /// closed agent must not release notices into whatever runs next, and the
    /// backlog it cannot drain must not keep the count the badge shows.
    pub(crate) fn discard_injections(&self) -> usize {
        let queued = self.injections.drain().len();
        self.notice_lock.discard_held();
        queued
    }

    /// Release the injections whose turn has come, and nothing earlier.
    ///
    /// The parked backlog drains first — a held item that arrived earlier must
    /// be delivered earlier — and at most one of them per
    /// [`DRAIN_INTERVAL_MS`](crate::notice_lock::DRAIN_INTERVAL_MS). Live
    /// traffic is never paced: a steer typed while the lock is on is the user
    /// waiting, not a backlog, and it drains at full speed behind the parked
    /// item.
    ///
    /// `None` means nothing was due. Everything returned must come back through
    /// [`Self::restore_due`] if the caller does not deliver it, or the release
    /// is spent on an item that never reached the model.
    pub fn take_due(&self, now: std::time::Instant) -> Option<Taken> {
        let mut items = Vec::new();
        let mut prev_due = None;
        if let Some(release) = self.notice_lock.take_held_due(now) {
            prev_due = release.prev_due;
            items.push(crate::notice_lock::TakenItem::held(release.entry));
        }
        for injection in self.injections.drain() {
            items.push(crate::notice_lock::TakenItem::queued(injection));
        }
        if items.is_empty() {
            return None;
        }
        Some(Taken::new(items, prev_due))
    }

    /// Put a [`Self::take_due`] batch back where it came from, deadline
    /// included. The exact inverse: a caller that declines a batch must leave
    /// the agent as if it had never looked.
    pub fn restore_due(&self, taken: Taken) {
        let mut queued = Vec::new();
        let mut parked = Vec::new();
        // Reversed, so each `push_front` below rebuilds the original order.
        for item in taken.items.into_iter().rev() {
            match item.origin {
                DueOrigin::Held => parked.push(item),
                DueOrigin::Queue => queued.push(item.injection),
            }
        }
        for item in parked {
            if let Some(entry) = item.entry {
                self.notice_lock.restore_held_due(entry, taken.prev_due);
            }
        }
        self.injections.restore(queued);
    }

    /// How long until the next paced release, `None` when none is scheduled.
    pub fn next_release_in(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.notice_lock.next_release_in(now)
    }

    pub(crate) fn begin_admission_turn(&self, message_id: String, turn: TurnId) -> bool {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cancelled = self
            .cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cancelled.contains(&message_id) {
            return false;
        }
        *admission = Some(TurnAdmission {
            message_id,
            turn,
            stop: None,
            progressed: false,
            cancelled: false,
        });
        true
    }

    pub(crate) fn reset_turn_usage(&self) {
        *self
            .turn_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Usage::default();
        *self
            .stream_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Usage::default();
    }

    pub(crate) fn set_stream_usage(&self, usage: Usage) {
        *self
            .stream_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = usage;
    }

    pub(crate) fn take_stream_usage(&self) -> Usage {
        std::mem::take(
            &mut *self
                .stream_usage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub(crate) fn record_turn_usage(&self, usage: Usage) {
        let mut total = self
            .turn_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *total += usage;
    }

    pub(crate) fn take_turn_usage(&self) -> Usage {
        let mut total = std::mem::take(
            &mut *self
                .turn_usage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        total += self.take_stream_usage();
        total
    }

    pub(crate) fn mark_admission_progressed(&self, message_id: &str, turn: &TurnId) {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cancelled = self
            .cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(admission) = admission
            .as_mut()
            .filter(|admission| admission.message_id == message_id && &admission.turn == turn)
            && !cancelled.contains(message_id)
        {
            admission.progressed = true;
        }
    }

    pub(crate) fn cancel_admission_turn(&self) -> Option<String> {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cancelled = self
            .cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admission = admission.as_mut()?;
        admission.cancelled = true;
        cancelled.insert(&admission.message_id);
        Some(admission.message_id.clone())
    }

    pub(crate) fn cancel_admission(&self, message_id: &str) {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cancelled = self
            .cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(current) = admission
            .as_mut()
            .filter(|current| current.message_id == message_id)
        {
            current.cancelled = true;
        }
        cancelled.insert(message_id);
    }

    pub(crate) fn admission_is_cancelled(&self, message_id: &str) -> bool {
        self.cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(message_id)
    }

    pub(crate) fn finish_admission_turn(
        &self,
        turn: &TurnId,
        stop: &TurnStop,
    ) -> Option<AdmissionFinish> {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cancelled = self
            .cancelled_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let finish = {
            let current = admission
                .as_mut()
                .filter(|admission| &admission.turn == turn)?;
            current.stop = Some(stop.clone());
            AdmissionFinish {
                message_id: current.message_id.clone(),
                terminal: current.progressed
                    || current.cancelled
                    || cancelled.contains(&current.message_id),
            }
        };
        if finish.terminal {
            admission.take();
        }
        Some(finish)
    }

    pub(crate) fn clear_admission_turn(&self, turn: &TurnId) {
        let mut admission = self
            .admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission
            .as_ref()
            .is_some_and(|admission| &admission.turn == turn)
        {
            admission.take();
        }
    }

    pub(crate) fn admission_turn(&self, message_id: &str) -> Option<TurnAdmission> {
        self.admission_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|admission| admission.message_id == message_id)
            .cloned()
    }

    pub(crate) fn set_current_turn(&self, turn: &TurnId) {
        let mut fence = self
            .turn_fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fence.current = Some(turn.clone());
        fence.terminal_error = None;
    }

    pub(crate) fn set_terminal_turn(&self, turn: &TurnId, stop: &TurnStop) {
        let mut fence = self
            .turn_fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fence.current = None;
        fence.terminal = Some(turn.clone());
        fence.terminal_error = match stop {
            TurnStop::Failed { message } => Some(message.clone()),
            TurnStop::Completed | TurnStop::Interrupted => None,
        };
    }

    pub(crate) fn seed_terminal_turn(&self, turn: &TurnId, stop: &TurnStop) {
        self.set_terminal_turn(turn, stop);
    }

    pub(crate) fn current_turn(&self) -> Option<TurnId> {
        self.turn_fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .clone()
    }

    pub(crate) fn terminal_turn(&self) -> Option<TurnId> {
        self.turn_fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .terminal
            .clone()
    }

    pub(crate) fn terminal_error(&self) -> Option<String> {
        self.turn_fence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .terminal_error
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn has_pending_user_messages(&self) -> bool {
        !self
            .pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    pub(crate) fn persisted_pending_user_messages(&self) -> Vec<(u64, Arc<Message>)> {
        self.pending_user_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|reservation| reservation.is_persisted())
            .map(|reservation| (reservation.seq(), reservation.message().clone()))
            .collect()
    }

    pub fn id(&self) -> AgentId {
        (*self.id_arc()).clone()
    }

    pub fn id_arc(&self) -> Arc<AgentId> {
        self.id
            .read()
            .map(|id| id.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn set_id(&self, new_id: AgentId) {
        if let Ok(mut guard) = self.id.write() {
            *guard = Arc::new(new_id);
        } else if let Err(poisoned) = self.id.write() {
            *poisoned.into_inner() = Arc::new(new_id);
        }
    }

    pub fn with_id<R>(&self, f: impl FnOnce(&AgentId) -> R) -> R {
        let guard = self.id.read().unwrap_or_else(|p| p.into_inner());
        f(&guard)
    }

    pub fn prompt(&self) -> Option<String> {
        self.prompt
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn set_prompt(&self, prompt: Option<String>) {
        if let Ok(mut guard) = self.prompt.write() {
            *guard = prompt;
        } else if let Err(p) = self.prompt.write() {
            *p.into_inner() = prompt;
        }
    }

    pub fn info(&self) -> AgentInfo {
        self.info
            .read()
            .map(|i| i.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn status(&self) -> AgentStatus {
        self.info().status
    }
    /// What the last (or running) fusion turn cost, per half. `None` when
    /// fusion is off for this agent — the common case, and the one a client
    /// must render exactly as it rendered before fusion existed.
    pub fn fusion_trace(&self) -> Option<crate::fusion::FusionTrace> {
        self.fusion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// A turn (or its post-block) currently owns this agent: `start_turn`'s
    /// supervisor holds `turn_lock` for the whole lifecycle, so "running" ==
    /// "the lock is taken". Because refusal to start and `is_running` are the
    /// same condition, a racing submit can never land a message with no turn
    /// behind it — it takes the steer path instead and is drained by the
    /// turn's end-of-turn injection check.
    pub fn is_running(&self) -> bool {
        self.turn_lock.try_lock().is_err()
    }

    pub fn request_stop(&self) {
        self.turn_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn turn_generation(&self) -> u64 {
        self.turn_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn invalidate_turn_generation(&self) {
        self.turn_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn clear_stop_request(&self) {
        self.stop_requested
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Update status/activity; the caller emits `AgentUpdated` through the session.
    /// A live status clears the finish stamp (a `continue`d agent is running
    /// again); a terminal one stamps it once.
    pub fn set_status(&self, status: AgentStatus, activity: Option<String>) {
        if let Ok(mut info) = self.info.write() {
            info.status = status;
            info.activity = activity;
            if status.is_live() {
                info.finished_at_ms = None;
            } else if info.finished_at_ms.is_none() {
                info.finished_at_ms = Some(pacode_types::now_ms());
            }
        }
    }

    /// Terminal `Failed` status: records the error text and stamps the finish
    /// time in one write. Poison-tolerant — this runs on panic-recovery paths
    /// where the lock may already be poisoned. The caller emits `AgentUpdated`.
    pub fn set_failed(&self, activity: Option<String>, error: String) {
        let mut info = self.info.write().unwrap_or_else(|p| p.into_inner());
        info.status = AgentStatus::Failed;
        info.activity = activity;
        info.error = Some(error);
        if info.finished_at_ms.is_none() {
            info.finished_at_ms = Some(pacode_types::now_ms());
        }
    }

    pub(crate) fn clear_error(&self) {
        self.info.write().unwrap_or_else(|p| p.into_inner()).error = None;
    }

    /// Record where the in-memory tail ends and the store begins, in message
    /// seqs. `None` means the tail already covers the whole conversation.
    ///
    /// Called right after the tail is built at resume or detach.
    pub(crate) fn seed_history_floor(&self, floor: Option<u64>) {
        *self
            .history_floor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = floor;
    }

    /// The store boundary: `seq < floor` are the messages older than the tail.
    pub fn history_floor(&self) -> Option<u64> {
        *self
            .history_floor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Next item seq for a store page, counting down from the tail's lowest so
    /// store items are always strictly older than tail items. `fetch_update`
    /// hands back the *previous* value, so the seq returned here is that value
    /// less one — returning it as-is would hand the first page the very seq the
    /// tail starts at. Saturates at 0 rather than wrapping: a wrapped seq would
    /// collide with a live item.
    fn next_older_item_seq(&self) -> u64 {
        self.older_item_seq
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| Some(current.saturating_sub(1)),
            )
            .map(|previous| previous.saturating_sub(1))
            .unwrap_or(0)
    }

    /// Read up to `max_items` transcript items older than the in-memory tail out
    /// of the store, and report whether anything older is left.
    ///
    /// Paging is driven by `history_floor` — a message-seq cursor the agent
    /// owns — and never by the caller's item cursor, so the two domains cannot
    /// be confused no matter what the client echoes back.
    ///
    /// Messages are the unit the store pages in, but `max_items` is a budget in
    /// items, and one message can project into several items. A page is
    /// therefore never split mid-message: cutting between a `tool_use` and its
    /// `tool_result` would strand a tool call that never resolves, and cutting
    /// anywhere else would either overrun the budget or drop items the next
    /// page could not reproduce. So the last message that does not fit is held
    /// back whole and `history_floor` stops in front of it.
    pub async fn load_older_history(
        &self,
        store: &pacode_store::Store,
        session_id: &pacode_types::SessionId,
        max_items: usize,
    ) -> (Vec<TranscriptItem>, bool) {
        use std::sync::Arc;

        let Some(mut cursor) = self.history_floor() else {
            // The tail already covers everything; there is no older window.
            return (Vec::new(), false);
        };
        if max_items == 0 {
            return (Vec::new(), cursor > 0);
        }

        let agent_id = self.id();
        let mut items: Vec<TranscriptItem> = Vec::new();
        let mut more = true;

        while more && items.len() < max_items {
            let want = max_items - items.len();
            // How many messages to ask for. Starts optimistic (one item per
            // message) and shrinks when a message turns out to be wider.
            let mut read_limit = want.min(u32::MAX as usize) as u32;

            loop {
                let rows = match store
                    .load_messages_before(session_id, &agent_id, Some(cursor), read_limit)
                    .await
                {
                    Ok(rows) => rows,
                    Err(error) => {
                        log::warn!("history page read failed for {agent_id}: {error}");
                        return (items, false);
                    }
                };
                if rows.is_empty() {
                    more = false;
                    break;
                }

                let first_seq = rows[0].seq;
                let tuples: Vec<(u64, Arc<pacode_types::Message>)> = rows
                    .iter()
                    .map(|row| (row.seq, Arc::new(row.message.clone())))
                    .collect();
                let projected = crate::transcript::history_to_items(&agent_id, &tuples);

                if projected.len() > want {
                    // Overran the item budget. Re-read proportionally fewer
                    // messages, using the ratio this window just revealed. Each
                    // retry strictly shrinks the read, so this terminates.
                    let shrunk =
                        ((rows.len() as u64 * want as u64) / projected.len() as u64).max(1);
                    let shrunk = u32::try_from(shrunk).unwrap_or(1);
                    if shrunk < read_limit {
                        read_limit = shrunk;
                        continue;
                    }
                    // Cannot shrink: one message is wider than the whole
                    // budget. Take it whole — a bounded overrun beats dropping
                    // the message or looping forever.
                } else if projected.is_empty() {
                    // A window of hidden messages projects to nothing. Consume
                    // it so the cursor still moves, then read further back.
                    cursor = first_seq;
                    break;
                }

                for entry in projected {
                    let mut item = entry.item;
                    item.seq = self.next_older_item_seq();
                    items.push(item);
                }
                cursor = first_seq;
                break;
            }
        }

        // The boundary is the oldest message actually consumed; anything below
        // it is still unread.
        *self
            .history_floor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cursor);
        (items, more && cursor > 0)
    }

    /// Last `limit` transcript items before `before_seq` from the in-memory tail;
    /// older items come from the store (see `load_older_history`).
    pub fn transcript_tail(
        &self,
        before_seq: Option<u64>,
        limit: usize,
    ) -> (Vec<TranscriptItem>, bool) {
        let has_older_in_store = self.history_floor().is_some();
        let transcript = match self.transcript.lock() {
            Ok(t) => t,
            Err(e) => e.into_inner(),
        };
        let filtered: Vec<TranscriptItem> = transcript
            .tail
            .iter()
            .filter(|item| match before_seq {
                Some(before) => item.seq < before,
                None => true,
            })
            .cloned()
            .collect();

        let total = filtered.len();
        let skip = total.saturating_sub(limit);
        let items: Vec<TranscriptItem> = filtered.into_iter().skip(skip).collect();

        // Either the tail itself has older items, or the store does. The old
        // `first.seq > 0` guess conflated the two and reported `has_more` for
        // a session whose oldest item happened to sit at seq 0.
        let has_more = skip > 0 || has_older_in_store;

        (items, has_more)
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;
