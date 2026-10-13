//! A subagent's side of the exchange: its open question, whether its running
//! turn still reads messages, and who stopped it. Every decision `agent send`,
//! `stop_agent` and the turn loop make about a subagent's messages is taken
//! under this one lock (see `rpc`).

use std::sync::Mutex;

use pacode_tools::{ParentReply, ToolError};
use pacode_types::{AgentId, CallId};
use tokio::sync::{Notify, oneshot};

use crate::agent::Agent;
use crate::inject::{Injection, InjectionQueue};

/// Questions in a row that may end without an answer before `ask_parent`
/// stops sending them: an orchestrator that keeps not answering is not woken
/// for one more.
pub(crate) const MAX_UNANSWERED_IN_ROW: u8 = 3;

/// Who stopped an agent. The orchestrator may resume an agent it stopped
/// itself; one the user stopped is the user's to resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopOrigin {
    User,
    Orchestrator,
}

/// One per agent; only a subagent ever has anything in it.
#[derive(Default)]
pub struct Mailbox {
    state: Mutex<MailboxState>,
    /// Woken when the agent opens a question or a turn starts on it: what a
    /// `send` waiting for the turn lock re-checks on, so it never waits out a
    /// turn that would have read (or asked for) its message.
    pub(super) changed: Notify,
}

#[derive(Default)]
struct MailboxState {
    question: Option<OpenQuestion>,
    /// The running turn decided to finish: a message now resumes the agent.
    sealed: bool,
    /// Set by a stop, cleared when the agent is resumed.
    stopped_by: Option<StopOrigin>,
    /// Questions in a row that ended without an answer; any message from the
    /// orchestrator resets it.
    unanswered_in_row: u8,
}

pub(super) struct OpenQuestion {
    /// The `AgentQuestion` as queued on the parent, kept to queue it again
    /// when the parent changes under it (detach).
    injection: Injection,
    call_id: CallId,
    /// The parent's history holds the question: the end of the parent's turn
    /// settles it.
    delivered: bool,
    reply: oneshot::Sender<ParentReply>,
}

/// What [`Mailbox::open`] did.
pub(super) enum Opened {
    Asked(oneshot::Receiver<ParentReply>),
    /// Too many questions in a row went unanswered; this one was not queued.
    NotAnswering,
}

/// What [`Mailbox::offer`] did with a message.
pub(super) enum Offer {
    Answered(CallId),
    Queued,
    /// The user stopped the agent: resuming it is the user's call.
    StoppedByUser,
    Ended,
}

impl Mailbox {
    fn state(&self) -> std::sync::MutexGuard<'_, MailboxState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A turn claimed the agent: it reads messages until it seals.
    pub(crate) fn unseal(&self) {
        self.state().sealed = false;
        self.changed.notify_waiters();
    }

    /// The turn is about to finish. `false` when a message is already queued:
    /// the turn owes the agent another one instead.
    pub(crate) fn seal_unless_message(&self, queue: &InjectionQueue) -> bool {
        let mut state = self.state();
        if queue.has_parent_message() {
            return false;
        }
        state.sealed = true;
        true
    }

    /// A stop is landing. Recorded before the turn is cancelled and under the
    /// lock `send` decides under, so no answer is handed to a call that is
    /// about to be dropped. A user stop is never downgraded by a later one.
    pub(crate) fn note_stop(&self, origin: StopOrigin) {
        let mut state = self.state();
        if state.stopped_by != Some(StopOrigin::User) {
            state.stopped_by = Some(origin);
        }
    }

    /// The agent is being resumed: the stop no longer stands.
    pub(crate) fn clear_stop(&self) {
        let mut state = self.state();
        state.stopped_by = None;
        state.unanswered_in_row = 0;
    }

    pub(crate) fn stopped_by(&self) -> Option<StopOrigin> {
        self.state().stopped_by
    }

    #[cfg(test)]
    pub(crate) fn has_open_question(&self) -> bool {
        self.state().question.is_some()
    }

    /// Open the question and queue it on `parent` in one step under the lock
    /// `send` answers under: an answer can never find the question without
    /// its injection, so retracting the injection after an answer is exact.
    pub(super) fn open(&self, parent: &Agent, question: Injection) -> Result<Opened, ToolError> {
        let Injection::AgentQuestion { call_id, .. } = &question else {
            return Err(ToolError::failed("not a question"));
        };
        let call_id = call_id.clone();
        let mut state = self.state();
        if state.question.is_some() {
            return Err(ToolError::invalid(
                "a question to the orchestrator is already waiting for its answer",
            ));
        }
        if state.unanswered_in_row >= MAX_UNANSWERED_IN_ROW {
            return Ok(Opened::NotAnswering);
        }
        let (reply, rx) = oneshot::channel();
        // Never parked by the notice lock (`Injection::is_holdable`).
        parent.injections.push(question.clone());
        state.question = Some(OpenQuestion {
            injection: question,
            call_id,
            delivered: false,
            reply,
        });
        drop(state);
        self.changed.notify_waiters();
        Ok(Opened::Asked(rx))
    }

    /// Take the question of `call_id` out, unanswered (the asking call is
    /// ending on its own).
    pub(super) fn take_question(&self, call_id: &CallId) -> bool {
        let mut state = self.state();
        if state
            .question
            .as_ref()
            .is_some_and(|q| &q.call_id == call_id)
        {
            state.question = None;
            return true;
        }
        false
    }

    pub(super) fn mark_delivered(&self, call_id: &CallId) {
        if let Some(question) = self
            .state()
            .question
            .as_mut()
            .filter(|q| &q.call_id == call_id)
        {
            question.delivered = true;
        }
    }

    /// End the open question with `reply` if the parent was shown it.
    pub(super) fn resolve_delivered(&self, reply: ParentReply) {
        self.resolve_if(|q| q.delivered, reply);
    }

    /// End the question of `call_id` with `reply`, delivered or not: the
    /// parent can no longer be shown it.
    pub(super) fn resolve_call(&self, call_id: &CallId, reply: ParentReply) {
        self.resolve_if(|q| &q.call_id == call_id, reply);
    }

    fn resolve_if(&self, which: impl Fn(&OpenQuestion) -> bool, reply: ParentReply) {
        let mut state = self.state();
        if !state.question.as_ref().is_some_and(which) {
            return;
        }
        let Some(question) = state.question.take() else {
            return;
        };
        match &reply {
            ParentReply::Answered(_) => state.unanswered_in_row = 0,
            ParentReply::Unanswered | ParentReply::ParentFailed | ParentReply::NotAnswering => {
                state.unanswered_in_row = state.unanswered_in_row.saturating_add(1);
            }
        }
        let _ = question.reply.send(reply);
    }

    /// The parent this question was queued on is gone (detach): the open
    /// question, marked undelivered, as the injection to queue on the new one.
    pub(super) fn undeliver(&self) -> Option<Injection> {
        let mut state = self.state();
        let question = state.question.as_mut()?;
        question.delivered = false;
        Some(question.injection.clone())
    }

    /// The one decision of `send`, under the lock the turn seals and a stop
    /// lands under.
    pub(super) fn offer(&self, agent: &Agent, text: &str) -> Offer {
        let mut state = self.state();
        match state.stopped_by {
            Some(StopOrigin::User) => return Offer::StoppedByUser,
            // Being stopped by the orchestrator itself: no answer for a call
            // that is about to be dropped; the stop is waited out and the
            // message resumes the agent.
            Some(StopOrigin::Orchestrator) => return Offer::Ended,
            None => {}
        }
        // An asking call that is already gone takes no answer: the message
        // goes on as if no question were open.
        if let Some(question) = state.question.take()
            && question
                .reply
                .send(ParentReply::Answered(text.to_string()))
                .is_ok()
        {
            state.unanswered_in_row = 0;
            return Offer::Answered(question.call_id);
        }
        if agent.is_running() && !state.sealed && !agent.stop_requested() {
            agent.injections.push(Injection::ParentMessage {
                text: text.to_string(),
            });
            state.unanswered_in_row = 0;
            return Offer::Queued;
        }
        Offer::Ended
    }
}

/// The asking agent of an `AgentQuestion`, with its call.
pub(super) fn question_key(injection: &Injection) -> Option<(&AgentId, &CallId)> {
    match injection {
        Injection::AgentQuestion { agent, call_id, .. } => Some((agent, call_id)),
        Injection::UserSteer { .. }
        | Injection::TaskFinished { .. }
        | Injection::TaskStalled { .. }
        | Injection::AgentFinished { .. }
        | Injection::SystemNotice(_)
        | Injection::StatusRequest
        | Injection::AgentStatus { .. }
        | Injection::ParentMessage { .. } => None,
    }
}

#[cfg(test)]
#[path = "mailbox_tests.rs"]
mod mailbox_tests;
