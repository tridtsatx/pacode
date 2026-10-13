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
//! - **stopped by the user** — refused: resuming it is the user's call;
//! - **ended** — no turn, a sealed one, or one the orchestrator is stopping:
//!   `send` takes the turn lock and resumes the agent with the message as its
//!   new input (a finished agent's history is reloaded from the store first).
//!   A report it ended with that the orchestrator has not read yet is taken
//!   back and returned with the call, so it cannot arrive after the resume
//!   posing as the reply. `Failed` is refused: the turn broke, and a new
//!   agent is the honest retry.
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
mod mailbox;
mod route;

use std::sync::Arc;

use pacode_tools::{SendOutcome, ToolError};
use pacode_types::{
    AGENT_MESSAGE_CAP_CHARS, AgentId, AgentMessageDirection, AgentStatus, Event, TranscriptKind,
};
use tokio::sync::OwnedMutexGuard;

use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;
pub(crate) use ask::ask_parent;
pub use mailbox::Mailbox;
use mailbox::Offer;
pub(crate) use mailbox::StopOrigin;
use route::post;
pub(crate) use route::{
    mark_delivered, reroute_questions, resolve_delivered, resolve_undeliverable,
};

/// `agent send` from `from` to its subagent `target`.
pub(crate) async fn send(
    session: &Arc<Session>,
    from: &Arc<Agent>,
    target: &AgentId,
    text: &str,
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
        match agent.mailbox.offer(&agent, &text) {
            Offer::Answered(call_id) => {
                // Before returning, not in the asker's cleanup: the parent's
                // next drain must not show it a question it just answered.
                from.injections.retract_question(&info.id, &call_id);
                return Ok(SendOutcome::Answered);
            }
            Offer::Queued => {
                post(session, from, item.clone());
                post(session, &agent, item);
                return Ok(SendOutcome::Delivered);
            }
            Offer::StoppedByUser => return Err(stopped_by_user(target)),
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
                // the message half-persisted or the agent half-resumed.
                let resume = tokio::spawn(resume(
                    session.clone(),
                    from.clone(),
                    agent.clone(),
                    turn,
                    text,
                    item,
                ));
                return match resume.await {
                    Ok(result) => result.map(|previous_report| SendOutcome::Resumed { previous_report }),
                    Err(error) => Err(ToolError::failed(format!(
                        "resuming agent {target} failed: {error}"
                    ))),
                };
            }
            _ = &mut changed => {}
        }
    }
}

fn stopped_by_user(agent: &AgentId) -> ToolError {
    ToolError::failed(format!(
        "agent {agent} was stopped by the user; ask the user before resuming it"
    ))
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
) -> Result<Option<String>, ToolError> {
    let _submit = session.submit_lock.lock().await;
    let id = agent.id();
    if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ToolError::failed("session is closed"));
    }
    if !session
        .agent(&id)
        .is_some_and(|current| Arc::ptr_eq(&current, &agent))
    {
        return Err(ToolError::failed(format!("agent not found: {id}")));
    }
    // The user may have stopped it while this call waited for the lock.
    if agent.mailbox.stopped_by() == Some(StopOrigin::User) {
        return Err(stopped_by_user(&id));
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
    // Held until the turn is started, as `continue_agent` does: a stop waits
    // for it, and one that began earlier already moved the generation, which
    // cancels the new turn at its start.
    let _messages = agent.message_write_lock.clone().lock_owned().await;
    let generation = agent.turn_generation();
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
    // The ended turn's finalization has run (this call holds the lock it ran
    // under), so its report is queued or parked on `from` by now, if unread.
    let previous_report = take_unread_report(&session, &from, &id);
    post(&session, &from, item.clone());
    post(&session, &agent, item);
    agent.mailbox.clear_stop();
    agent.clear_stop_request();
    agent.set_status(AgentStatus::Idle, None);
    session.events.emit(Event::AgentUpdated(agent.info()));
    crate::turn::lifecycle::start_claimed_turn(
        session.clone(),
        agent.clone(),
        turn,
        generation,
        None,
    );
    Ok(previous_report)
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
        | Injection::AgentQuestion { .. } => None,
    })
}

#[cfg(test)]
pub(crate) mod seal_hooks;

#[cfg(test)]
mod edge_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod window_tests;
