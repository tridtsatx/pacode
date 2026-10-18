//! Two-way talk between the orchestrator and its subagents.
//!
//! * orchestrator → subagent: `agent message` queues [`Injection::ParentMessage`]
//!   on the running subagent; `agent answer` resolves its pending question.
//! * subagent → orchestrator: `ask_parent` registers one pending question on the
//!   asking agent ([`AgentComm`]) and pushes [`Injection::AgentQuestion`] to the
//!   parent, waking an idle main agent the way a finished subagent does.
//!
//! This file holds the pure state and rendering; the session-level flow lives in
//! `agent_comm_flow.rs`.

#[cfg(test)]
#[path = "agent_comm_tests.rs"]
mod agent_comm_tests;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use pacode_tools::ToolError;
use pacode_tools::builtin::agent::MAX_MESSAGE_CHARS;
use pacode_types::{AgentId, AgentInfo};
use tokio::sync::oneshot;

use crate::inject::Injection;

/// Most unread orchestrator messages one subagent may have queued.
pub const MAX_QUEUED_PARENT_MESSAGES: usize = 16;

/// Why a message, answer or question was refused. The text goes back to the model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommError {
    #[error("text is required")]
    EmptyText,
    #[error("text is {len} characters; the limit is {limit}")]
    TextTooLong { len: usize, limit: usize },
    #[error("agent not found: {0}")]
    NotFound(AgentId),
    #[error("the main agent cannot be messaged this way")]
    IsMain,
    #[error(
        "agent {0} has already finished or stopped; spawn a new agent with the context it needs"
    )]
    NotRunning(AgentId),
    #[error(
        "agent {agent} already has {cap} unread messages from you; let it catch up before sending more"
    )]
    QueueFull { agent: AgentId, cap: usize },
    #[error("only subagents can ask the orchestrator")]
    NoParent,
    #[error("the orchestrator is gone")]
    ParentGone,
    #[error("question {0} is still waiting for an answer; wait for it before asking another")]
    QuestionPending(String),
    #[error("you have used all {0} questions; make a reasonable assumption and state it")]
    AskLimit(u32),
    #[error("agent {0} has no pending question")]
    NoPendingQuestion(AgentId),
    #[error("agent {agent} is waiting on question {expected}, not {got}")]
    WrongQuestion {
        agent: AgentId,
        expected: String,
        got: String,
    },
}

impl From<CommError> for ToolError {
    fn from(e: CommError) -> Self {
        ToolError::Failed(e.to_string())
    }
}

/// How a pending question ended for the blocked `ask_parent` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskOutcome {
    Answered(String),
    Cancelled,
}

/// Where an answer went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerRoute {
    /// A blocked `ask_parent` call received it as its tool result.
    Waiter,
    /// The question was non-blocking: the caller must inject the answer.
    Later,
}

/// Result of offering a plain message to a possibly-waiting agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRoute {
    /// A blocked `ask_parent` call took the message as its answer; do not queue it.
    Consumed(String),
    /// Queue it as a normal message.
    Queue,
}

/// Whether a message may be accepted for an agent in this state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Accept,
    Reject,
}

/// A subagent only reads messages while its turn is running; a finished or
/// stopped one has dropped its history and cannot be revived.
pub fn delivery(live: bool, running: bool) -> Delivery {
    if live && running {
        Delivery::Accept
    } else {
        Delivery::Reject
    }
}

/// An idle main agent is woken for a new injection exactly as `AgentFinished`
/// does it; a running one reads it at its next step.
pub fn should_wake(parent_is_main: bool, parent_running: bool) -> bool {
    parent_is_main && !parent_running
}

/// Trim and bound the text of a message or answer.
pub fn check_text(text: &str) -> Result<String, CommError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CommError::EmptyText);
    }
    let len = trimmed.chars().count();
    if len > MAX_MESSAGE_CHARS {
        return Err(CommError::TextTooLong {
            len,
            limit: MAX_MESSAGE_CHARS,
        });
    }
    Ok(trimmed.to_string())
}

/// First line of `text`, at most `max` characters, for transcript notices.
pub fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max && text.lines().count() <= 1 {
        return line.to_string();
    }
    let head: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

static NEXT_QUESTION: AtomicU64 = AtomicU64::new(1);

fn next_question_id() -> String {
    format!("q{}", NEXT_QUESTION.fetch_add(1, Ordering::Relaxed))
}

struct Pending {
    id: String,
    /// `Some` while a tool call is blocked on the answer.
    tx: Option<oneshot::Sender<AskOutcome>>,
}

#[derive(Default)]
struct CommState {
    /// Questions asked over the agent's lifetime.
    asked: u32,
    pending: Option<Pending>,
}

/// Per-agent question bookkeeping: at most one pending question, and a lifetime
/// count of questions asked.
#[derive(Default)]
pub struct AgentComm {
    state: Mutex<CommState>,
}

impl AgentComm {
    fn lock(&self) -> std::sync::MutexGuard<'_, CommState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register a question. `max` is the lifetime cap. With `wait` the receiver
    /// resolves when the question is answered or dropped.
    pub fn begin_question(
        &self,
        max: u32,
        wait: bool,
    ) -> Result<(String, Option<oneshot::Receiver<AskOutcome>>), CommError> {
        let mut state = self.lock();
        if let Some(p) = &state.pending {
            return Err(CommError::QuestionPending(p.id.clone()));
        }
        if state.asked >= max {
            return Err(CommError::AskLimit(max));
        }
        let id = next_question_id();
        let (tx, rx) = oneshot::channel();
        state.asked += 1;
        if wait {
            state.pending = Some(Pending {
                id: id.clone(),
                tx: Some(tx),
            });
            Ok((id, Some(rx)))
        } else {
            state.pending = Some(Pending {
                id: id.clone(),
                tx: None,
            });
            Ok((id, None))
        }
    }

    pub fn pending_id(&self) -> Option<String> {
        self.lock().pending.as_ref().map(|p| p.id.clone())
    }

    pub fn asked(&self) -> u32 {
        self.lock().asked
    }

    /// Answer the pending question `question_id` of agent `agent`.
    pub fn answer(
        &self,
        agent: &AgentId,
        question_id: &str,
        text: &str,
    ) -> Result<AnswerRoute, CommError> {
        let mut state = self.lock();
        let Some(pending) = state.pending.as_ref() else {
            return Err(CommError::NoPendingQuestion(agent.clone()));
        };
        if pending.id != question_id {
            return Err(CommError::WrongQuestion {
                agent: agent.clone(),
                expected: pending.id.clone(),
                got: question_id.to_string(),
            });
        }
        let Some(done) = state.pending.take() else {
            return Err(CommError::NoPendingQuestion(agent.clone()));
        };
        match done.tx {
            Some(tx) => {
                // A waiter that vanished is not an error: it was cancelled.
                let _ = tx.send(AskOutcome::Answered(text.to_string()));
                Ok(AnswerRoute::Waiter)
            }
            None => Ok(AnswerRoute::Later),
        }
    }

    /// A plain message resolves a blocked question, handing it the text.
    pub fn deliver_message(&self, text: &str) -> MessageRoute {
        let mut state = self.lock();
        let waiting = state.pending.as_ref().is_some_and(|p| p.tx.is_some());
        if !waiting {
            return MessageRoute::Queue;
        }
        match state.pending.take() {
            Some(Pending { id, tx: Some(tx) }) => {
                let _ = tx.send(AskOutcome::Answered(text.to_string()));
                MessageRoute::Consumed(id)
            }
            Some(other) => {
                state.pending = Some(other);
                MessageRoute::Queue
            }
            None => MessageRoute::Queue,
        }
    }

    /// A plain message also settles a non-blocking question.
    pub fn clear_unwaited(&self) -> Option<String> {
        let mut state = self.lock();
        match state.pending.take() {
            Some(Pending { id, tx: None }) => Some(id),
            Some(other) => {
                state.pending = Some(other);
                None
            }
            None => None,
        }
    }

    /// Remove `question_id` if it is still pending (timeout). `false` = it was
    /// answered or dropped first.
    pub fn expire(&self, question_id: &str) -> bool {
        let mut state = self.lock();
        if state.pending.as_ref().is_some_and(|p| p.id == question_id) {
            state.pending = None;
            true
        } else {
            false
        }
    }

    /// Drop the pending question (agent stopped, interrupted or finished); a
    /// blocked call wakes with [`AskOutcome::Cancelled`].
    pub fn cancel(&self) -> Option<String> {
        let pending = self.lock().pending.take()?;
        if let Some(tx) = pending.tx {
            let _ = tx.send(AskOutcome::Cancelled);
        }
        Some(pending.id)
    }
}

/// `true` for the injections that count against the per-agent message cap.
pub fn is_parent_message(injection: &Injection) -> bool {
    matches!(injection, Injection::ParentMessage { .. })
}

/// `<message_from_orchestrator>` as the subagent sees it.
pub fn render_parent_message(text: &str, cap_chars: usize) -> String {
    let capped = pacode_types::truncate_head_tail(text, cap_chars);
    format!(
        "<message_from_orchestrator>\n{capped}\n</message_from_orchestrator>\nTake this into account and continue your task."
    )
}

/// `<question_from_agent>` as the orchestrator sees it.
pub fn render_agent_question(
    agent: &AgentInfo,
    question_id: &str,
    question: &str,
    options: &[String],
    cap_chars: usize,
) -> String {
    let capped = pacode_types::truncate_head_tail(question, cap_chars);
    let mut body = format!(
        "<question_from_agent id=\"{question_id}\" agent=\"{}\">\n{capped}\n",
        agent.name
    );
    if !options.is_empty() {
        body.push_str("Options:\n");
        for option in options {
            body.push_str(&format!("- {option}\n"));
        }
    }
    body.push_str("</question_from_agent>\n");
    body.push_str(&format!(
        "Answer briefly with the `agent` tool: action `answer`, agent_id `{}`, question_id `{question_id}`, text. \
         A plain `message` to that agent also resolves it. Do not ask the user unless you truly cannot decide.",
        agent.id
    ));
    body
}

/// `<answer_from_orchestrator>` for a non-blocking question.
pub fn render_parent_answer(question_id: &str, text: &str, cap_chars: usize) -> String {
    let capped = pacode_types::truncate_head_tail(text, cap_chars);
    format!(
        "<answer_from_orchestrator question=\"{question_id}\">\n{capped}\n</answer_from_orchestrator>"
    )
}
