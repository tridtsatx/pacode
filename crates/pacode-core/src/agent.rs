//! `Agent`: history, status and transcript bookkeeping for the main agent and each
//! subagent.

use std::sync::{Arc, Mutex, RwLock};

use pacode_tools::ToolRegistry;
use pacode_types::{AgentId, AgentInfo, AgentStatus, Message, TranscriptItem};
use tokio_util::sync::CancellationToken;

use crate::inject::InjectionQueue;
use crate::transcript::TranscriptState;

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
    pub fn for_model(&self) -> Vec<Message> {
        let mut out = Vec::new();
        if let Some(summary) = &self.summary {
            out.push(
                Message::user(format!("[Previous conversation summary]\n{summary}"))
                    .with_kind(pacode_types::MessageKind::CompactionSummary),
            );
        }
        for m in &self.messages {
            out.push((**m).clone());
        }
        out
    }
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
    /// Turns run so far (subagent cap `agents.max_turns`).
    pub turns: Mutex<u32>,
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
            turns: Mutex::new(0),
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
            turns: Mutex::new(0),
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
