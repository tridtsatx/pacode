//! `Agent`: history, status and transcript bookkeeping for the main agent and each
//! subagent.

use std::sync::{Arc, Mutex, RwLock};

use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentInfo, AgentStatus, CallId, ContentBlock, Message, Role, TranscriptItem,
};
use tokio_util::sync::CancellationToken;

use crate::inject::InjectionQueue;
use crate::transcript::TranscriptState;

/// Content of a synthesized `tool_result` answering a `tool_use` that never
/// produced one: the turn was interrupted or died mid-dispatch, or persisted
/// history was loaded with the pairing already broken. `is_error` is set so
/// adapters can mark it as a failed call.
pub(crate) const INTERRUPTED_TOOL_RESULT: &str = "[interrupted]";

/// The model-visible history of one agent: a compaction summary (if any) plus the
/// messages after it. Messages are `Arc` so a forked subagent shares them.
#[derive(Clone, Default)]
pub struct History {
    pub messages: Vec<Arc<Message>>,
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
        let seq = self.next_seq;
        self.next_seq += 1;
        self.estimated_tokens = self.estimated_tokens.saturating_add(msg.estimate_tokens());
        let msg = Arc::new(msg);
        self.messages.push(msg.clone());
        (seq, msg)
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
        for id in uses
            .iter()
            .copied()
            .filter(|id| !answered.contains(*id))
        {
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
    pub injections: InjectionQueue,
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
    /// One turn at a time — and what `is_running` reads. `Arc` so
    /// `start_turn` can claim it with an owned guard and move it into the
    /// supervisor task, which holds it across the spawned turn's whole
    /// lifetime: run, token cleanup, `TurnEnded` emission, and the post-block
    /// (re-drive check + terminal landing).
    pub turn_lock: Arc<tokio::sync::Mutex<()>>,
    pub transcript: Mutex<TranscriptState>,
    /// The prompt a subagent was spawned with (persisted).
    pub prompt: RwLock<Option<String>>,
    /// History snapshot as it stood before the current turn began (for turn detach).
    pub history_before_turn: Mutex<Option<History>>,
    /// Turn-scoped prompt cache (invalidation point: turn start).
    pub prompt_cache: Mutex<Option<crate::prompt::TurnPromptCache>>,
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
            injections: InjectionQueue::default(),
            tools: RwLock::new(tools),
            cancel: Mutex::new(None),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            transcript: Mutex::new(TranscriptState::new(history_page as usize * 2)),
            prompt: RwLock::new(prompt),
            history_before_turn: Mutex::new(None),
            prompt_cache: Mutex::new(None),
        }
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
            injections: InjectionQueue::default(),
            tools: RwLock::new(tools),
            cancel: Mutex::new(None),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            transcript: Mutex::new(transcript),
            prompt: RwLock::new(prompt),
            history_before_turn: Mutex::new(None),
            prompt_cache: Mutex::new(None),
        }
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
        self.stop_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    /// Last `limit` transcript items before `before_seq` from the in-memory tail;
    /// older items come from the store (see `transcript::history_to_items`).
    pub fn transcript_tail(
        &self,
        before_seq: Option<u64>,
        limit: usize,
    ) -> (Vec<TranscriptItem>, bool) {
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

        let has_more = if let Some(first) = items.first() {
            first.seq > 0 || skip > 0
        } else {
            before_seq.is_some_and(|b| b > 0)
        };

        (items, has_more)
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;
