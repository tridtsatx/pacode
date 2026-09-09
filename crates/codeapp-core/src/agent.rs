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
        todo!("History::for_model")
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
        let _ = (status, activity);
        todo!("Agent::set_status")
    }

    /// Last `limit` transcript items before `before_seq` from the in-memory tail;
    /// older items come from the store (see `transcript::history_to_items`).
    pub fn transcript_tail(
        &self,
        before_seq: Option<u64>,
        limit: usize,
    ) -> (Vec<TranscriptItem>, bool) {
        let _ = (before_seq, limit);
        todo!("Agent::transcript_tail")
    }
}
