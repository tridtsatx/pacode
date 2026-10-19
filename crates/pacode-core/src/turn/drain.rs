//! The injection drain the turn loop runs between steps (points B and D, and
//! before each request): fold what is due into history as one message, and
//! leave a user message that is not this turn's for the turn it starts.

use std::sync::Arc;

use crate::CoreError;
use crate::agent::Agent;
use crate::notice_lock::{DueOrigin, Taken};
use crate::session::Session;

use super::persist_history_message;

pub(super) enum DrainOutcome {
    Idle,
    Continue,
    DeferredAdmission,
}

pub(super) async fn drain_injections(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    admission: Option<&str>,
    defer_admissions: bool,
) -> Result<DrainOutcome, CoreError> {
    let Some(taken) = agent.take_due(std::time::Instant::now()) else {
        return Ok(DrainOutcome::Idle);
    };
    if taken
        .items
        .iter()
        .any(|item| item.origin == DueOrigin::Held)
    {
        session.emit_notice_lock(agent, agent.notice_lock.held());
    }
    let is_foreign_admission = |item: &crate::inject::Injection| {
        matches!(
            item,
            crate::inject::Injection::UserSteer {
                admission: Some(candidate)
            } if Some(candidate.as_str()) != admission
        )
    };
    if defer_admissions
        && taken
            .items
            .iter()
            .any(|item| is_foreign_admission(&item.injection))
    {
        agent.restore_due(taken);
        return Ok(DrainOutcome::DeferredAdmission);
    }

    let prev_due = taken.prev_due;
    let mut deferred = Vec::new();
    let mut active = Vec::new();
    for item in taken.items {
        if is_foreign_admission(&item.injection) {
            deferred.push(item);
        } else {
            active.push(item.injection);
        }
    }

    if let Err(error) = hide_other_admission_messages(session, agent, admission).await {
        agent.restore_due(Taken::new(deferred, prev_due));
        // What was taken is lost with the failed turn (re-queueing it would
        // spin the next turn on the same broken store), but a question must
        // not vanish: its subagent would wait for an answer forever.
        crate::rpc::resolve_undeliverable(session, &active);
        return Err(error);
    }

    if active.is_empty() {
        agent.restore_due(Taken::new(deferred, prev_due));
        return Ok(DrainOutcome::Idle);
    }

    let has_user_steer = active
        .iter()
        .any(|item| matches!(item, crate::inject::Injection::UserSteer { .. }));
    let renderable = active
        .into_iter()
        .filter(|item| !matches!(item, crate::inject::Injection::UserSteer { .. }))
        .collect::<Vec<_>>();
    if renderable.is_empty() {
        agent.restore_due(Taken::new(deferred, prev_due));
        return Ok(if has_user_steer {
            DrainOutcome::Continue
        } else {
            DrainOutcome::Idle
        });
    }

    let injection_msg =
        crate::inject::render_injections(&renderable, session.config.context.injection_cap_chars);
    if let Err(error) = persist_history_message(session, agent, injection_msg).await {
        agent.restore_due(Taken::new(deferred, prev_due));
        crate::rpc::resolve_undeliverable(session, &renderable);
        return Err(error);
    }
    // In the history now: a completed turn from here on has seen them.
    crate::rpc::mark_delivered(session, &renderable);
    agent.restore_due(Taken::new(deferred, prev_due));
    Ok(DrainOutcome::Continue)
}

pub(super) async fn hide_other_admission_messages(
    session: &Session,
    agent: &Agent,
    current_admission: Option<&str>,
) -> Result<(), CoreError> {
    // Both queries are keyed by (session, agent): this runs on every injection
    // drain, several times per model step, and the agent's own history can be
    // tens of thousands of rows long.
    let mut message_seqs = session
        .store
        .cancelled_message_seqs(&session.id, &agent.id())
        .await?;
    for record in session
        .store
        .load_unresolved_message_admissions(&session.id, &agent.id())
        .await?
    {
        if current_admission == Some(record.message_id.as_str()) {
            continue;
        }
        if let Some(seq) = record.message_seq {
            message_seqs.insert(seq);
        }
    }
    if message_seqs.is_empty() {
        return Ok(());
    }

    let mut history = agent.history.lock().unwrap_or_else(|p| p.into_inner());
    let mut index = 0;
    while index < history.message_seqs.len() {
        if message_seqs.contains(&history.message_seqs[index]) {
            history.message_seqs.remove(index);
            history.messages.remove(index);
        } else {
            index += 1;
        }
    }
    history.estimated_tokens = history
        .messages
        .iter()
        .map(|message| message.estimate_tokens())
        .sum();
    Ok(())
}
