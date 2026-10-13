//! Orchestrator <-> subagent messaging (spec §8): `agent send` one way,
//! `ask_parent` the other.
//!
//! What `send` does depends on where the subagent is, read under its
//! [`Mailbox`] lock in one step:
//! - **asking** — blocked in `ask_parent`: the message is the answer;
//! - **accepting** — a turn owns the agent and has not decided to finish: the
//!   message is queued as `<orchestrator_message>` and read at the next step.
//!   A turn that is about to finish checks the queue under the same lock
//!   before it seals itself (`seal_unless_message`), so a message that lands
//!   in its last moments re-drives it instead of being dropped with it;
//! - **stopped by the user** — refused, unless the user, asked by the
//!   `agent` tool, agreed to lift that very stop ([`Consent`]): resuming it
//!   is the user's call;
//! - **ended** — no turn, a sealed one, or one the orchestrator is stopping:
//!   `send` takes the turn lock and resumes the agent with the message as its
//!   new input (a finished agent's history is reloaded from the store first).
//!   A report it ended with that the orchestrator has not read yet is taken
//!   back and returned with the call, so it cannot arrive after the resume
//!   posing as the reply. `Failed` is refused: the turn broke, and a new
//!   agent is the honest retry. A stop or a close that lands while the resume
//!   is under way wins: the resume reads the agent's turn generation before
//!   its checks, and takes effect only if no stop moved it since
//!   ([`Mailbox::lift_stop`]).
//!
//! An answer counts once the asking call took it: a stop can cancel the call
//! between the hand-over and its next poll, and that call's result says
//! cancelled — so the answer was not given, and `send` decides again (as a
//! message to a stopped agent). A `send` the orchestrator's interruption
//! drops after its message took effect is reported at its next step instead
//! (see `landed`), report included.
//!
//! A question ends exactly once, because every resolver takes it out of the
//! mailbox under that lock: answered by `send`; unanswered when the parent's
//! turn ends on the model's terms with the question in its history; failed
//! when that turn fails, or when the question cannot be put into it at all;
//! cancelled when the asking turn is stopped (the tool future is dropped and
//! the asker's guard cleans up). No timers: a turn the user interrupted or
//! preempted with a message leaves the question open for the next one, and
//! after [`mailbox::MAX_UNANSWERED_IN_ROW`] unanswered questions the next one
//! is not sent at all. A detach hands open questions to the new orchestrator.
//! A daemon restart drops the wait with the turn; the call ends as interrupted
//! like any other tool call that was in flight.

mod ask;
mod landed;
mod mailbox;
mod route;

use std::sync::Arc;

use pacode_tools::{SendOutcome, ToolError};
use pacode_types::{
    AGENT_MESSAGE_CAP_CHARS, AgentId, AgentMessageDirection, AgentStatus, Event, TranscriptKind,
};
use tokio::sync::{OwnedMutexGuard, oneshot};

use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;
pub(crate) use ask::ask_parent;
pub use mailbox::Mailbox;
use mailbox::Offer;
pub(crate) use mailbox::{Consent, Lift, StopOrigin};
use route::post;
pub(crate) use route::{
    mark_delivered, reroute_questions, resolve_delivered, resolve_undeliverable,
};

/// `agent send` from `from` to its subagent `target`; `consent` is the
/// user's leave to resume it if they stopped it.
pub(crate) async fn send(
    session: &Arc<Session>,
    from: &Arc<Agent>,
    target: &AgentId,
    text: &str,
    consent: Consent,
) -> Result<SendOutcome, ToolError> {
    let agent = session
        .agent(target)
        .ok_or_else(|| ToolError::failed(format!("agent not found: {target}")))?;
    let info = agent.info();
    if info.parent.as_ref() != Some(&from.id()) {
        return Err(ToolError::invalid(format!(
            "agent {target} is not your subagent"
        )));
    }
    let text = pacode_types::truncate_head_tail(text.trim(), AGENT_MESSAGE_CAP_CHARS);
    let item = TranscriptKind::AgentMessage {
        subagent: info.id.clone(),
        subagent_name: info.name.clone(),
        direction: AgentMessageDirection::ToSubagent,
        text: text.clone(),
        reply: None,
    };
    loop {
        // Armed before the check, so a question opened or a turn started
        // between the check and the wait still wakes it.
        let changed = agent.mailbox.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        match agent.mailbox.offer(&agent, &text, consent) {
            Offer::Answered { call_id, taken } => {
                // Before returning, not in the asker's cleanup: the parent's
                // next drain must not show it a question it just answered.
                from.injections.retract_question(&info.id, &call_id);
                if landed::Awaited::new(taken, landed::answered, session, &agent)
                    .get()
                    .await
                    .is_ok()
                {
                    return Ok(SendOutcome::Answered);
                }
                // A stop dropped the asking call before it took the answer.
                continue;
            }
            Offer::Queued => {
                post(session, from, item.clone());
                post(session, &agent, item);
                return Ok(SendOutcome::Delivered);
            }
            Offer::StoppedByUser => return Err(ToolError::failed(kept_stopped(target, consent))),
            Offer::Ended => {}
        }
        // Waits only for a turn that is finishing or being stopped — both
        // unwind without the parent — and re-checks if the agent starts a turn
        // or asks meanwhile. Turn lock before `submit_lock` (in `resume`): the
        // order every turn holds them in.
        tokio::select! {
            turn = agent.turn_lock.clone().lock_owned() => {
                // From here the resume runs to its end on a task of its own:
                // the orchestrator being interrupted mid-call must not leave
                // the message half-persisted or the agent half-resumed — and
                // what it did reaches the orchestrator either way.
                let (tx, rx) = oneshot::channel();
                let resumed = landed::Awaited::new(rx, landed::resumed, session, &agent);
                let (session, from, agent) = (session.clone(), from.clone(), agent.clone());
                tokio::spawn(async move {
                    let result =
                        resume(session.clone(), from, agent.clone(), turn, text, item, consent).await;
                    landed::deliver(tx, result, landed::resumed, &session, &agent);
                });
                return match resumed.get().await {
                    Ok(result) => result.map(|previous_report| SendOutcome::Resumed { previous_report }),
                    // The task ended without a result: it panicked.
                    Err(_) => Err(ToolError::failed(format!("resuming agent {target} failed"))),
                };
            }
            _ = &mut changed => {}
        }
    }
}

/// Why the orchestrator may not resume `agent`, which the user stopped
/// (`send` and `continue`).
pub(crate) fn kept_stopped(agent: &AgentId, consent: Consent) -> String {
    match consent {
        Consent::Given(_) => format!(
            "agent {agent} was stopped again while the user was being asked; it stays stopped"
        ),
        Consent::Own | Consent::Absent => format!(
            "agent {agent} was stopped by the user; send or continue again to ask the user \
             whether to resume it"
        ),
    }
}

/// Re-drive an ended subagent with `text` as its new input; `turn` is its turn
/// lock, which the new turn inherits. Returns the report it had ended with,
/// when the orchestrator had not read it yet.
async fn resume(
    session: Arc<Session>,
    from: Arc<Agent>,
    agent: Arc<Agent>,
    turn: OwnedMutexGuard<()>,
    text: String,
    item: TranscriptKind,
    consent: Consent,
) -> Result<Option<String>, ToolError> {
    let _submit = session.submit_lock.lock().await;
    let id = agent.id();
    if !session
        .agent(&id)
        .is_some_and(|current| Arc::ptr_eq(&current, &agent))
    {
        return Err(ToolError::failed(format!("agent not found: {id}")));
    }
    let info = agent.info();
    match info.status {
        AgentStatus::Failed => {
            let why = info.error.map(|e| format!(" ({e})")).unwrap_or_default();
            return Err(ToolError::failed(format!(
                "agent {id} failed{why} and cannot be resumed; spawn a new agent for the work"
            )));
        }
        AgentStatus::Idle
        | AgentStatus::Thinking
        | AgentStatus::RunningTool
        | AgentStatus::WaitingApproval
        | AgentStatus::WaitingParent
        | AgentStatus::Finished
        | AgentStatus::Stopped => {}
    }
    #[cfg(test)]
    hooks::run(&id, hooks::Point::ResumeChecked);
    // Held until the turn is started, as `continue_agent` does. A stop moves
    // the generation before it waits for this lock, and a close sets
    // `closed` before it moves it, so the generation read here, with the
    // checks after it, draws the line: a stop or a close already in is seen
    // now, and one that lands later is seen where the resume takes effect —
    // it wins either way. An orchestrator stop that finished first is simply
    // what the message resumes the agent from.
    let _messages = agent.message_write_lock.clone().lock_owned().await;
    let generation = agent.turn_generation();
    if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ToolError::failed("session is closed"));
    }
    if agent.mailbox.held_by_user(consent) {
        return Err(ToolError::failed(kept_stopped(&id, consent)));
    }
    let cleared = {
        let history = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        history.messages.is_empty() && history.summary.is_none()
    };
    if cleared {
        // A finished subagent dropped its history when it reported.
        let history = crate::core::open::load_subagent_history(
            &session.store,
            &session.id,
            &id,
            agent.prompt(),
            session.config.session.history_page,
        )
        .await
        .map_err(|e| ToolError::failed(e.to_string()))?;
        *agent.history.lock().unwrap_or_else(|p| p.into_inner()) = history;
    }
    agent.commit_pending_user_messages();
    let message = crate::inject::render_injections(
        &[Injection::ParentMessage { text }],
        session.config.context.injection_cap_chars,
    );
    // Straight to the store: `append_agent_message` takes the message lock
    // this function already holds.
    let reservation = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .reserve(message);
    if let Err(error) = session
        .store
        .append_message(&session.id, &id, reservation.seq(), reservation.message())
        .await
    {
        agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .rollback_reserved(&reservation);
        return Err(ToolError::failed(error.to_string()));
    }
    agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .commit_reserved(&reservation);
    #[cfg(test)]
    hooks::run(&id, hooks::Point::ResumeCommitted);
    // Every await is behind: the resume takes effect here, or yields to a
    // stop or a close that came in meanwhile.
    let lifted = if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
        Lift::Superseded
    } else {
        agent.mailbox.lift_stop(&agent, generation)
    };
    let Lift::Lifted { was } = lifted else {
        if cleared {
            // Back to how a finished agent is kept; the store has it all.
            agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
        return Err(superseded(&session, &agent));
    };
    // The ended turn's finalization has run (this call holds the lock it ran
    // under), so its report is queued or parked on `from` by now, if unread.
    let previous_report = take_unread_report(&session, &from, &id);
    post(&session, &from, item.clone());
    post(&session, &agent, item);
    agent.set_status(AgentStatus::Idle, None);
    session.events.emit(Event::AgentUpdated(agent.info()));
    crate::turn::lifecycle::start_claimed_turn(
        session.clone(),
        agent.clone(),
        turn,
        generation,
        None,
    );
    if was.is_some() {
        persist_stop_origin(&session, &agent).await;
    }
    Ok(previous_report)
}

/// Why a resume that already persisted its message did not take effect.
fn superseded(session: &Session, agent: &Agent) -> ToolError {
    let id = agent.id();
    if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
        return ToolError::failed("session is closed");
    }
    let by = match agent.mailbox.stopped_by() {
        Some(StopOrigin::User) => " by the user",
        Some(StopOrigin::Orchestrator) | None => "",
    };
    ToolError::failed(format!(
        "agent {id} was stopped{by} while your message was being delivered; the message is \
         in its history, for when it is resumed"
    ))
}

/// Bring the stored stop origin in line with `agent`'s mailbox, for a reopen
/// to restore. Called under the agent's message lock, as every write of it
/// is, so the store applies them in the order the origin changed.
pub(crate) async fn persist_stop_origin(session: &Session, agent: &Agent) {
    let origin = agent.mailbox.stopped_by().map(StopOrigin::as_str);
    if let Err(error) = session
        .store
        .set_agent_stopped_by(&session.id, &agent.id(), origin)
        .await
    {
        log::warn!(
            "failed to persist the stop origin of agent {} in {}: {error}",
            agent.id(),
            session.id
        );
    }
}

/// Take `agent`'s unread report off `parent` — queued, or parked by the
/// notice lock — and hand back its text.
fn take_unread_report(session: &Session, parent: &Arc<Agent>, agent: &AgentId) -> Option<String> {
    let mut reports = parent.injections.take_agent_finished(agent);
    let held = parent.notice_lock.take_agent_finished(agent);
    if !held.is_empty() {
        session.emit_notice_lock(parent, parent.notice_lock.held());
    }
    reports.extend(held);
    reports.into_iter().rev().find_map(|report| match report {
        Injection::AgentFinished { answer, .. } => Some(answer),
        Injection::UserSteer { .. }
        | Injection::TaskFinished { .. }
        | Injection::TaskStalled { .. }
        | Injection::SystemNotice(_)
        | Injection::StatusRequest
        | Injection::AgentStatus { .. }
        | Injection::ParentMessage { .. }
        | Injection::AgentQuestion { .. }
        | Injection::SendLanded { .. }
        | Injection::PlanReminder { .. } => None,
    })
}

#[cfg(test)]
pub(crate) mod hooks;

#[cfg(test)]
mod consent_tests;
#[cfg(test)]
mod edge_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
pub(crate) mod harness;
#[cfg(test)]
mod race_tests;
#[cfg(test)]
mod reopen_tests;
#[cfg(test)]
mod window_tests;
