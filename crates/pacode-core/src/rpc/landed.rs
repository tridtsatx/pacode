//! An `agent send` whose call is dropped — its orchestrator was interrupted —
//! after the message took effect: it answered the question, or it resumed
//! the agent and took back a report the orchestrator had not read. The call's
//! result says cancelled, so the orchestrator is told what really happened at
//! its next step instead; without that it would send the message again, and
//! the report would be lost.
//!
//! Exactly one side tells it: the receiver, dropped before the effect arrived,
//! closes its channel and looks once more — a value already in it is the
//! receiver's to report; one sent after the close bounces back to the sender,
//! which reports it.

use std::sync::Arc;

use pacode_tools::{SendOutcome, ToolError};
use tokio::sync::oneshot;

use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;

/// What a value arriving on a `send`'s channel did, if it did anything.
pub(super) type Effect<T> = fn(T) -> Option<SendOutcome>;

/// The asking call took the answer.
pub(super) fn answered(_taken: ()) -> Option<SendOutcome> {
    Some(SendOutcome::Answered)
}

/// The resume ran to its end; only a successful one did anything.
pub(super) fn resumed(result: Result<Option<String>, ToolError>) -> Option<SendOutcome> {
    result
        .ok()
        .map(|previous_report| SendOutcome::Resumed { previous_report })
}

/// The receiving end of an effect a `send` waits for. Dropped before it was
/// read, it reports a value that already arrived (see the module docs).
pub(super) struct Awaited<T> {
    rx: oneshot::Receiver<T>,
    effect: Effect<T>,
    session: Arc<Session>,
    /// The subagent the message went to.
    agent: Arc<Agent>,
    read: bool,
}

impl<T> Awaited<T> {
    pub(super) fn new(
        rx: oneshot::Receiver<T>,
        effect: Effect<T>,
        session: &Arc<Session>,
        agent: &Arc<Agent>,
    ) -> Self {
        Self {
            rx,
            effect,
            session: session.clone(),
            agent: agent.clone(),
            read: false,
        }
    }

    pub(super) async fn get(mut self) -> Result<T, oneshot::error::RecvError> {
        let value = (&mut self.rx).await;
        self.read = true;
        value
    }
}

impl<T> Drop for Awaited<T> {
    fn drop(&mut self) {
        if self.read {
            return;
        }
        self.rx.close();
        if let Ok(value) = self.rx.try_recv()
            && let Some(outcome) = (self.effect)(value)
        {
            tell_parent(&self.session, &self.agent, outcome);
        }
    }
}

/// Send `value` to the `send` waiting on `tx`; if its call is gone, report
/// what the value did in its place.
pub(super) fn deliver<T>(
    tx: oneshot::Sender<T>,
    value: T,
    effect: Effect<T>,
    session: &Session,
    agent: &Agent,
) {
    if let Err(value) = tx.send(value)
        && let Some(outcome) = effect(value)
    {
        tell_parent(session, agent, outcome);
    }
}

/// Queue `outcome` for whoever holds `agent`'s parent's place now. No turn is
/// started for it: the call was dropped because the orchestrator was
/// interrupted, and it reads this at its next step, before it could resend.
fn tell_parent(session: &Session, agent: &Agent, outcome: SendOutcome) {
    let info = agent.info();
    let Some(parent) = info.parent.as_ref().and_then(|id| session.agent(id)) else {
        return;
    };
    parent.injections.push(Injection::SendLanded {
        agent: info.id,
        name: info.name,
        outcome,
    });
}
