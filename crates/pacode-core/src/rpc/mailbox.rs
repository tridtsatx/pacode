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

/// Orchestrator messages one subagent may have queued and unread at once. A
/// `send` past it is refused: the agent reads its queue at its next step, and
/// an orchestrator that keeps sending meanwhile is flooding, not steering.
pub(crate) const MAX_QUEUED_PARENT_MESSAGES: usize = 16;

/// Who stopped an agent. The orchestrator may resume an agent it stopped
/// itself; one the user stopped is the user's to resume. Stored with the
/// agent (`rpc::persist_stop_origin`), so a daemon restart keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopOrigin {
    User,
    Orchestrator,
}

impl StopOrigin {
    /// As the store records it, so a reopen restores it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Orchestrator => "orchestrator",
        }
    }

    pub(crate) fn parse(stored: &str) -> Option<Self> {
        match stored {
            "user" => Some(Self::User),
            "orchestrator" => Some(Self::Orchestrator),
            _ => None,
        }
    }
}

/// What entitles a resume to lift a stop the user made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consent {
    /// The user is the one resuming.
    Own,
    /// The user, asked, agreed to lift their stop — this one, by its number
    /// ([`Mailbox::user_stop`]): a stop made since is not covered.
    Given(u64),
    /// Nobody asked the user.
    Absent,
}

impl From<Option<u64>> for Consent {
    /// The orchestrator's: the stop the user agreed to lift, if it asked.
    fn from(given: Option<u64>) -> Self {
        given.map_or(Self::Absent, Self::Given)
    }
}

impl Consent {
    fn covers(self, stop: u64) -> bool {
        match self {
            Self::Own => true,
            Self::Given(given) => given == stop,
            Self::Absent => false,
        }
    }
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
    /// Stops recorded so far: the number a user's leave to lift one names.
    stops: u64,
}

pub(super) struct OpenQuestion {
    /// The `AgentQuestion` as queued on the parent, kept to queue it again
    /// when the parent changes under it (detach).
    injection: Injection,
    call_id: CallId,
    /// The parent's history holds the question: the end of the parent's turn
    /// settles it.
    delivered: bool,
    reply: oneshot::Sender<Reply>,
}

/// How a question ended, as the asking call receives it.
pub(super) struct Reply {
    pub(super) reply: ParentReply,
    /// Set for an answer: the asking call confirms on it that it took the
    /// answer. A stop can drop the call before it does, and an answer the
    /// call never took was never given — its result says cancelled, so the
    /// orchestrator must not hear `Answered` (`send` decides again instead).
    pub(super) taken: Option<oneshot::Sender<()>>,
}

/// What [`Mailbox::open`] did.
pub(super) enum Opened {
    Asked(oneshot::Receiver<Reply>),
    /// Too many questions in a row went unanswered; this one was not queued.
    NotAnswering,
}

/// What [`Mailbox::offer`] did with a message.
pub(super) enum Offer {
    /// Handed to the asking call of `call_id`; `taken` resolves once the call
    /// took it, and fails if a stop dropped the call first.
    Answered {
        call_id: CallId,
        taken: oneshot::Receiver<()>,
    },
    Queued,
    /// [`MAX_QUEUED_PARENT_MESSAGES`] messages are already waiting unread.
    QueueFull,
    /// The user stopped the agent: resuming it is the user's call.
    StoppedByUser,
    Ended,
}

/// What [`Mailbox::lift_stop`] found.
pub(crate) enum Lift {
    /// The stop no longer stands; `was` is who had stopped the agent.
    Lifted { was: Option<StopOrigin> },
    /// A stop or a close landed after the resume read its generation: it
    /// stands, and the resume does not take effect.
    Superseded,
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

    /// A stop is landing on `agent` (this mailbox's owner). Recorded before
    /// the turn is cancelled and under the lock `send` decides under, so no
    /// answer is handed to a call that is about to be dropped; and in one
    /// step with a move of the agent's turn generation, so a resume deciding
    /// under this lock ([`Self::lift_stop`]) either sees the stop or ran
    /// before it. A user stop is never downgraded by a later one.
    pub(crate) fn note_stop(&self, agent: &Agent, origin: StopOrigin) {
        let mut state = self.state();
        if state.stopped_by != Some(StopOrigin::User) {
            state.stopped_by = Some(origin);
        }
        state.stops += 1;
        agent.invalidate_turn_generation();
    }

    /// A resume of `agent` that read `generation` before its checks is about
    /// to take effect: the stop is lifted — its origin, the stop request, and
    /// the unanswered count (resuming is the orchestrator engaging again) —
    /// unless a stop or a close moved the generation since; that one stands.
    ///
    /// A close moves the generation without this lock, so the request is
    /// cleared first and the generation read again after: a close that
    /// slipped in between gets its request back.
    pub(crate) fn lift_stop(&self, agent: &Agent, generation: u64) -> Lift {
        let mut state = self.state();
        if agent.turn_generation() != generation {
            return Lift::Superseded;
        }
        agent.clear_stop_request();
        if agent.turn_generation() != generation {
            agent.request_stop();
            return Lift::Superseded;
        }
        state.unanswered_in_row = 0;
        Lift::Lifted {
            was: state.stopped_by.take(),
        }
    }

    /// The number of the stop the user made, while it stands: what the user
    /// is asked about, and what their leave ([`Consent::Given`]) names.
    pub(crate) fn user_stop(&self) -> Option<u64> {
        let state = self.state();
        (state.stopped_by == Some(StopOrigin::User)).then_some(state.stops)
    }

    /// Whether a stop the user made stands against a resume with `consent`.
    pub(crate) fn held_by_user(&self, consent: Consent) -> bool {
        let state = self.state();
        state.stopped_by == Some(StopOrigin::User) && !consent.covers(state.stops)
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
            // A turn that broke says nothing about whether the orchestrator
            // would have answered.
            ParentReply::ParentFailed => {}
            ParentReply::Unanswered | ParentReply::NotAnswering => {
                state.unanswered_in_row = state.unanswered_in_row.saturating_add(1);
            }
        }
        let _ = question.reply.send(Reply { reply, taken: None });
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
    pub(super) fn offer(&self, agent: &Agent, text: &str, consent: Consent) -> Offer {
        let mut state = self.state();
        match state.stopped_by {
            Some(StopOrigin::User) if !consent.covers(state.stops) => {
                return Offer::StoppedByUser;
            }
            // Being stopped: no answer for a call that is about to be
            // dropped; the stop is waited out and the message resumes the
            // agent — one the user stopped only with their leave.
            Some(StopOrigin::User | StopOrigin::Orchestrator) => return Offer::Ended,
            None => {}
        }
        // An asking call that is already gone takes no answer: the message
        // goes on as if no question were open.
        if let Some(question) = state.question.take() {
            let (taken, taken_rx) = oneshot::channel();
            let reply = Reply {
                reply: ParentReply::Answered(text.to_string()),
                taken: Some(taken),
            };
            if question.reply.send(reply).is_ok() {
                state.unanswered_in_row = 0;
                return Offer::Answered {
                    call_id: question.call_id,
                    taken: taken_rx,
                };
            }
        }
        if agent.is_running() && !state.sealed && !agent.stop_requested() {
            let queued = agent.injections.push_limited(
                Injection::ParentMessage {
                    text: text.to_string(),
                },
                MAX_QUEUED_PARENT_MESSAGES,
                |item| matches!(item, Injection::ParentMessage { .. }),
            );
            if !queued {
                return Offer::QueueFull;
            }
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
        | Injection::ParentMessage { .. }
        | Injection::SendLanded { .. }
        | Injection::PlanReminder { .. } => None,
    }
}

#[cfg(test)]
#[path = "mailbox_tests.rs"]
mod mailbox_tests;
