//! A turn's user admission: claiming it in the store, restoring its message,
//! its terminal state, and parking it for recovery when the claim fails.

use std::sync::Arc;

use pacode_types::{TurnId, TurnStop};

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

pub(super) async fn claim_admission_for_turn(
    session: &Session,
    agent: &Agent,
    admission: &str,
) -> Result<Option<pacode_store::MessageAdmission>, pacode_store::StoreError> {
    let Some(record) = session
        .store
        .get_message_admission(&session.id, admission)
        .await?
    else {
        return Ok(None);
    };
    let turn_id = TurnId::new(format!("trn_{admission}"));
    if record.agent != agent.id() {
        return Ok(None);
    }
    if record
        .turn_id
        .as_ref()
        .is_some_and(|persisted| persisted != &turn_id)
    {
        log::warn!(
            "admission {admission} of session {} is recorded under turn {:?} instead of {turn_id}: leaving it unclaimed",
            session.id,
            record.turn_id
        );
        return Ok(None);
    }
    match record.state {
        pacode_store::MessageAdmissionState::Admitted => {
            if !session
                .store
                .mark_message_admission_running(&session.id, admission, turn_id.as_str())
                .await?
            {
                return Ok(None);
            }
            Ok(Some(record))
        }
        pacode_store::MessageAdmissionState::Running => Ok(Some(record)),
        pacode_store::MessageAdmissionState::Terminal => Ok(None),
    }
}

pub(super) async fn restore_admission_message(
    session: &Session,
    agent: &Agent,
    admission: &pacode_store::MessageAdmission,
) -> Result<(), CoreError> {
    let Some(seq) = admission.message_seq else {
        return Ok(());
    };
    if agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .message_seqs
        .contains(&seq)
    {
        return Ok(());
    }
    if let Some(message) = session.load_message_row(&admission.agent, seq).await? {
        agent
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore_row(seq, message);
    }
    Ok(())
}

pub(super) fn terminal_stop(stop: &TurnStop) -> &'static str {
    match stop {
        TurnStop::Completed => "completed",
        TurnStop::Interrupted => "interrupted",
        TurnStop::Failed { .. } => "failed",
    }
}

/// A cancel fence outranks whatever the turn reported: the store keeps the
/// first terminal write, so finalizing a fenced admission as `completed` would
/// lock in the opposite of what the interrupt asked for.
pub(super) fn persisted_terminal_stop(fenced: bool, stop: &TurnStop) -> &'static str {
    if fenced {
        "interrupted"
    } else {
        terminal_stop(stop)
    }
}

pub(super) fn spawn_mark_admissions_terminal(
    session: &Session,
    admissions: Vec<String>,
    terminal_stop: &'static str,
) {
    if admissions.is_empty() {
        return;
    }
    let store = session.store.clone();
    let session_id = session.id.clone();
    tokio::spawn(async move {
        for admission in admissions {
            let _ = store
                .mark_message_admission_terminal(&session_id, &admission, Some(terminal_stop))
                .await;
        }
    });
}

pub(super) fn remove_user_admission(agent: &Agent, admission: &str) {
    let remaining = agent
        .injections
        .drain()
        .into_iter()
        .filter(|item| {
            !matches!(
                item,
                crate::inject::Injection::UserSteer {
                    admission: Some(candidate)
                } if candidate.as_str() == admission
            )
        })
        .collect::<Vec<_>>();
    agent.injections.restore(remaining);
}

/// Keep an admission this turn could not claim: back onto the head of the
/// recovery queue, plus a steer so an idle agent is woken instead of leaving
/// the message parked until the next resume. A stop or a newer turn owns the
/// agent now, and both drain the recovery queue on their own. The wake follows
/// the queue insertion, so a store that keeps refusing the claim retries once
/// and then waits — a steer per attempt would spin turns against a dead store.
pub(super) fn park_admission_for_recovery(
    session: &Session,
    agent: &Arc<Agent>,
    generation: u64,
    admission: &str,
) {
    if agent.stop_requested() || agent.turn_generation() != generation {
        return;
    }
    if !session.queue_recovery_admission_first(&agent.id(), admission.to_owned()) {
        return;
    }
    agent.injections.push(crate::inject::Injection::UserSteer {
        admission: Some(admission.to_owned()),
    });
}
