//! `Agent`: history, status and transcript bookkeeping for the main agent and each
//! subagent.

use std::sync::{Arc, Mutex, RwLock};

use codeapp_tools::ToolRegistry;
use codeapp_types::{AgentId, AgentInfo, AgentStatus, Message, TranscriptItem};
use tokio_util::sync::CancellationToken;

use crate::inject::InjectionQueue;
use crate::transcript::TranscriptState;

/// The model-visible history of one agent: a compaction summary (if any) plus the
/// messages after it. Messages are `Arc` so a forked subagent shares them.
#[derive(Default)]
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
                    .with_kind(codeapp_types::MessageKind::CompactionSummary),
            );
        }
        for m in &self.messages {
            out.push((**m).clone());
        }
        out
    }
}

pub struct Agent {
    pub id: AgentId,
    pub info: RwLock<AgentInfo>,
    pub history: Mutex<History>,
    pub injections: InjectionQueue,
    /// Tools this agent may call (main: all; subagent: subset without `agent`).
    pub tools: ToolRegistry,
    /// Cancellation token of the running turn, if any.
    pub cancel: Mutex<Option<CancellationToken>>,
    /// One turn at a time.
    pub turn_lock: tokio::sync::Mutex<()>,
    pub transcript: Mutex<TranscriptState>,
    /// The prompt a subagent was spawned with (persisted).
    pub prompt: Option<String>,
    /// Turns run so far (subagent cap `agents.max_turns`).
    pub turns: Mutex<u32>,
}

impl Agent {
    pub fn info(&self) -> AgentInfo {
        self.info
            .read()
            .map(|i| i.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn status(&self) -> AgentStatus {
        self.info().status
    }

    pub fn is_running(&self) -> bool {
        self.cancel.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    /// Update status/activity; the caller emits `AgentUpdated` through the session.
    pub fn set_status(&self, status: AgentStatus, activity: Option<String>) {
        if let Ok(mut info) = self.info.write() {
            info.status = status;
            info.activity = activity;
            if !status.is_live() && info.finished_at_ms.is_none() {
                info.finished_at_ms = Some(codeapp_types::now_ms());
            }
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
