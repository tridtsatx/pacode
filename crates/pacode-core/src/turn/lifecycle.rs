//! Turn spawning and error notification lifecycle helpers.

use std::collections::HashSet;
use std::sync::Arc;
#[cfg(test)]
use std::sync::{Mutex as StdMutex, OnceLock};

use pacode_types::{AgentStatus, Event, Role, TurnId, TurnStop, Usage};
use tokio::sync::OwnedMutexGuard;
#[cfg(test)]
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::CoreError;
use crate::agent::Agent;
use crate::notice_lock::{DueOrigin, Taken};
use crate::session::Session;

#[cfg(test)]
struct LifecycleGate {
    expected_agent: StdMutex<Option<String>>,
    reached: Semaphore,
    release: Semaphore,
    /// One-shot. A turn now passes two gate points (subagent finalization and
    /// the window before the lock is released), and a gate that fired at both
    /// would hand the test two arrivals to tell apart — the earlier one
    /// arriving after it had already released the second.
    fired: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
static LIFECYCLE_GATE: OnceLock<StdMutex<Option<Arc<LifecycleGate>>>> = OnceLock::new();

#[cfg(test)]
fn install_lifecycle_gate(session: &Session, agent: &Agent) -> Arc<LifecycleGate> {
    let gate = Arc::new(LifecycleGate {
        expected_agent: StdMutex::new(Some(format!(
            "{}:{}",
            session.id.as_str(),
            agent.id().as_str()
        ))),
        reached: Semaphore::new(0),
        release: Semaphore::new(0),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    *LIFECYCLE_GATE
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(gate.clone());
    gate
}

#[cfg(test)]
async fn wait_for_lifecycle_gate(session: &Session, agent: &Agent) {
    let Some(gate) = LIFECYCLE_GATE.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }) else {
        return;
    };
    let expected = format!("{}:{}", session.id.as_str(), agent.id().as_str());
    let matches = gate
        .expected_agent
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_deref()
        .is_some_and(|expected_owner| expected_owner == expected);
    // One-shot: a turn passes two gate points now, and firing at both would
    // leave a test holding two arrivals it cannot tell apart.
    if matches && !gate.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
        gate.reached.add_permits(1);
        let _ = gate.release.acquire().await;
    }
}

/// Handles turn interruption: lands the agent on a resting status (main:
/// `Idle`; subagents: `Stopped` — they have no live state without a turn) and
/// emits `AgentUpdated`. The log/hook/`TurnEnded` tail runs in the spawn
/// supervisor after the cancel token is cleared.
pub(crate) fn handle_interrupted(session: &Arc<Session>, agent: &Arc<Agent>) -> TurnStop {
    // `stop_agent`/`interrupt_all` raised `stop_requested` before cancelling;
    // mid-unwind status writes (e.g. permission-restore) may flap the visible
    // status, so the flag — not the status — decides: `Stopped` survives for
    // `continue` to re-drive. A subagent has no resting live state at all:
    // `Idle` with no turn behind it is a permanent fake "live" (it keeps the
    // session busy and the rail blinking), so it lands `Stopped` too —
    // history kept, resumable via `continue_agent`.
    let status = if agent.id().is_main() && !agent.stop_requested() {
        AgentStatus::Idle
    } else {
        AgentStatus::Stopped
    };
    agent.set_status(status, None);
    session.events.emit(Event::AgentUpdated(agent.info()));
    TurnStop::Interrupted
}

/// Surface a turn failure in the transcript (error notice item) so clients and
/// `pacode run` see why nothing happened.
pub(crate) fn emit_failure_notice(session: &Session, agent: &Agent, message: &str) {
    let seq = agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .next_seq();
    let item = pacode_types::TranscriptItem {
        seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: pacode_types::TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Error,
            text: format!("turn failed: {message}"),
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemAdded(item));
}

/// Log line + TurnEnd plugin hook + `Event::TurnEnded` — emitted by the spawn
/// supervisor AFTER the cancel token is cleared, so an observer reacting to
/// `TurnEnded` never sees the agent still "running". `usage` is `None` only
/// when the turn task died before it could report one.
async fn emit_turn_end(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    turn_id: &TurnId,
    start_instant: std::time::Instant,
    usage: Option<Usage>,
    stop: &TurnStop,
) {
    let duration_ms = start_instant.elapsed().as_millis() as u64;
    let output_tokens = usage.map(|u| u.output_tokens).unwrap_or(0);
    match stop {
        TurnStop::Failed { message } => {
            log::warn!(
                "turn end: session={} agent={} turn={} stop=Failed duration={}ms error={message}",
                session.id,
                agent.id(),
                turn_id,
                duration_ms
            );
        }
        TurnStop::Completed | TurnStop::Interrupted => {
            log::info!(
                "turn end: session={} agent={} turn={} stop={stop:?} duration={}ms",
                session.id,
                agent.id(),
                turn_id,
                duration_ms
            );
        }
    }
    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::TurnEnd {
            duration_ms,
            output_tokens,
        })
        .await;
    session.events.emit(Event::TurnEnded {
        agent: agent.id(),
        turn: turn_id.clone(),
        usage,
        stop: stop.clone(),
    });
}

/// Shared exit for a `TurnStop::Failed` from `run_turn`: transcript notice and
/// a resting status on the main agent (a subagent keeps its live status for
/// the spawn post-block, which stamps `Failed` and reports the error to the
/// parent). The `TurnEnded` tail runs in the supervisor.
pub(crate) fn finish_failed_turn(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    message: String,
) -> TurnStop {
    emit_failure_notice(session, agent, &message);
    if agent.id().is_main() {
        // Same flag-first rule as `handle_interrupted`: a `stop_agent` or
        // `close_session` racing the failure keeps `Stopped`.
        let status = if agent.stop_requested() {
            AgentStatus::Stopped
        } else {
            AgentStatus::Idle
        };
        agent.set_status(status, None);
        session.events.emit(Event::AgentUpdated(agent.info()));
    }
    TurnStop::Failed { message }
}

/// The turn task died (panic or abort) before `run_turn` could land a terminal
/// status — the supervisor reports the crash itself so the agent cannot stay
/// "running" forever: transcript notice and `Failed` on the main agent (a
/// subagent is stamped by the shared post-block so the parent report carries
/// the same error text). Returns the synthesized stop.
fn report_turn_crash(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    err: &tokio::task::JoinError,
) -> TurnStop {
    let message = if err.is_panic() {
        format!("turn task panicked: {err}")
    } else {
        format!("turn task aborted: {err}")
    };
    emit_failure_notice(session, agent, &message);
    if agent.id().is_main() && !agent.stop_requested() {
        agent.set_failed(Some("turn task panicked".to_string()), message.clone());
        session.events.emit(Event::AgentUpdated(agent.info()));
    }
    TurnStop::Failed { message }
}

/// Spawn `run_turn` as a tokio task, storing the cancel token on the agent. Returns
/// immediately. When the turn ends, the token is cleared and, for subagents, the
/// parent gets an `Injection::AgentFinished` and the agent's history is persisted
/// and dropped from memory (info + summary stay).
pub fn start_turn(session: Arc<Session>, agent: Arc<Agent>) {
    let generation = agent.turn_generation();
    // A stopped subagent stays stopped: `stop_agent` can land while a spawn is
    // still wiring up the token, and a fresh token would outrun the stop. Only
    // `Session::continue_agent` restarts one — it clears the request first.
    if !agent.id().is_main() && agent.stop_requested() {
        // The flag can outlive a status flap (`continue_agent` stamping `Idle`
        // while a `stop_agent` is in flight, or a spawn-time stop): a live
        // status with no turn behind it is a zombie card that keeps the
        // session busy and the rail blinking forever, so force the terminal
        // landing `Session::stop_agent` performs.
        if agent.status().is_live() {
            agent.set_status(AgentStatus::Stopped, None);
            let info = agent.info();
            // Through the residency guard: a `/clear` may drop this agent
            // before the spawned write runs.
            tokio::spawn(async move {
                session.publish_agent(&agent, info).await;
            });
        }
        return;
    }

    if agent.stop_requested() {
        return;
    }
    let Some(turn_guard) = try_claim_turn(&agent) else {
        return;
    };
    start_claimed_turn(session, agent, turn_guard, generation, None);
}

pub(crate) fn request_turn(session: Arc<Session>, agent: Arc<Agent>) {
    if agent.stop_requested() {
        return;
    }
    let generation = agent.turn_generation();
    let Some(turn_guard) = try_claim_turn(&agent) else {
        return;
    };
    if agent.turn_generation() != generation || agent.stop_requested() {
        return;
    }

    let Some(taken) = agent.take_due(std::time::Instant::now()) else {
        return;
    };
    if taken
        .items
        .iter()
        .any(|item| item.origin == DueOrigin::Held)
    {
        session.emit_notice_lock(&agent, agent.notice_lock.held());
    }
    let prev_due = taken.prev_due;
    let mut seen = HashSet::new();
    let mut admission = None;
    let mut remaining: Vec<crate::notice_lock::TakenItem> = Vec::new();

    let mut cancelled = Vec::new();
    for item in taken.items {
        // Matched by reference: `restore_due` has to put an item back in the
        // deque it came from, and moving the injection out of the wrapper would
        // leave only half of it — the origin is what says whether the item was
        // paced or live.
        match &item.injection {
            crate::inject::Injection::UserSteer {
                admission: Some(candidate),
            } => {
                let candidate = candidate.clone();
                if !seen.insert(candidate.clone()) {
                    continue;
                }
                if agent.admission_is_cancelled(&candidate) {
                    cancelled.push(candidate);
                } else if admission.is_none() {
                    admission = Some(candidate);
                } else {
                    remaining.push(crate::notice_lock::TakenItem::queued(
                        crate::inject::Injection::UserSteer {
                            admission: Some(candidate),
                        },
                    ));
                }
            }
            _ => remaining.push(item),
        }
    }

    if agent.turn_generation() != generation || agent.stop_requested() {
        let mut interrupted = cancelled;
        if let Some(candidate) = admission {
            interrupted.push(candidate);
        }
        let mut retry = Vec::new();
        let keep_non_admissions = !agent.stop_requested();
        for item in remaining {
            match item.injection {
                crate::inject::Injection::UserSteer {
                    admission: Some(candidate),
                } => interrupted.push(candidate),
                _ if keep_non_admissions => retry.push(item),
                _ => {}
            }
        }
        tokio::spawn(async move {
            session
                .mark_admissions_terminal(interrupted, "interrupted")
                .await;
            agent.restore_due(Taken::new(retry, prev_due));
            drop(turn_guard);
            session.emit_session_status();
        });
        return;
    }

    // A parked backlog that is *not* in this batch is not work: it is the
    // user's own backlog, and a turn started for one burns a model round trip
    // to learn nothing. What this batch released is.
    let has_work = !remaining.is_empty();
    agent.restore_due(Taken::new(remaining, prev_due));
    if admission.is_none() && !has_work && agent.injections.is_empty() {
        if !cancelled.is_empty() {
            tokio::spawn(async move {
                session
                    .mark_admissions_terminal(cancelled, "interrupted")
                    .await;
                drop(turn_guard);
                session.emit_session_status();
            });
        }
        return;
    }
    spawn_mark_admissions_terminal(&session, cancelled, "interrupted");
    start_claimed_turn(session, agent, turn_guard, generation, admission);
}

/// Re-drive the agent for a parked backlog.
///
/// A parked notice is the user's own backlog: a turn started for one burns a
/// model round trip to learn nothing, and the pacing is the promise the badge
/// shows. So the wake is a timer, not a push — raced against a push, because a
/// steer the user is waiting on must never sit behind that backlog. When
/// nothing is parked this is an ordinary re-drive, which is what every other
/// caller of [`request_turn`] already does.
fn spawn_locked_redrive(session: Arc<Session>, agent: Arc<Agent>) {
    if !agent.notice_lock.wants_work() {
        request_turn(session, agent);
        return;
    }
    tokio::spawn(async move {
        match agent.next_release_in(std::time::Instant::now()) {
            Some(remaining) => {
                tokio::select! {
                    _ = tokio::time::sleep(remaining) => {}
                    _ = agent.injections.wait_nonempty() => {}
                }
            }
            None => agent.injections.wait_nonempty().await,
        }
        request_turn(session, agent);
    });
}

pub(crate) async fn start_turn_or_steer(
    session: Arc<Session>,
    agent: Arc<Agent>,
    admission: Option<&str>,
) -> Result<(), crate::CoreError> {
    if let Some(admission) = admission
        && agent.admission_is_cancelled(admission)
    {
        session
            .mark_admissions_terminal([admission.to_owned()], "interrupted")
            .await;
        return Ok(());
    }
    if agent.stop_requested() {
        if let Some(admission) = admission {
            session
                .mark_admissions_terminal([admission.to_owned()], "interrupted")
                .await;
        }
        return Ok(());
    }
    let generation = agent.turn_generation();
    if let Some(turn_guard) = try_claim_turn(&agent) {
        if agent.turn_generation() != generation
            || agent.stop_requested()
            || admission.is_some_and(|id| agent.admission_is_cancelled(id))
        {
            if let Some(admission) = admission {
                session
                    .mark_admissions_terminal([admission.to_owned()], "interrupted")
                    .await;
            }
            return Ok(());
        }
        start_claimed_turn(
            session,
            agent,
            turn_guard,
            generation,
            admission.map(str::to_owned),
        );
        return Ok(());
    }

    if agent.turn_generation() == generation
        && !agent.stop_requested()
        && !admission.is_some_and(|id| agent.admission_is_cancelled(id))
    {
        agent.injections.push(crate::inject::Injection::UserSteer {
            admission: admission.map(str::to_owned),
        });
    } else if let Some(admission) = admission {
        session
            .mark_admissions_terminal([admission.to_owned()], "interrupted")
            .await;
    }
    Ok(())
}

fn try_claim_turn(agent: &Agent) -> Option<OwnedMutexGuard<()>> {
    agent.turn_lock.clone().try_lock_owned().ok()
}

async fn claim_admission_for_turn(
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

async fn restore_admission_message(
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

fn terminal_stop(stop: &TurnStop) -> &'static str {
    match stop {
        TurnStop::Completed => "completed",
        TurnStop::Interrupted => "interrupted",
        TurnStop::Failed { .. } => "failed",
    }
}

/// A cancel fence outranks whatever the turn reported: the store keeps the
/// first terminal write, so finalizing a fenced admission as `completed` would
/// lock in the opposite of what the interrupt asked for.
fn persisted_terminal_stop(fenced: bool, stop: &TurnStop) -> &'static str {
    if fenced {
        "interrupted"
    } else {
        terminal_stop(stop)
    }
}

fn spawn_mark_admissions_terminal(
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

fn remove_user_admission(agent: &Agent, admission: &str) {
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
fn park_admission_for_recovery(
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

/// Shared exit for a turn that never reached `run_turn`: release the turn, and
/// start the next one when this turn left work behind.
async fn unwind_unclaimed_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    turn_guard: OwnedMutexGuard<()>,
) {
    *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let redrive =
        (!agent.injections.is_empty() || agent.notice_lock.wants_work()) && !agent.stop_requested();
    if !redrive {
        *agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }
    drop(turn_guard);
    session.emit_session_status();
    if redrive {
        spawn_locked_redrive(session.clone(), agent.clone());
    }
}

pub(crate) fn start_claimed_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    turn_guard: OwnedMutexGuard<()>,
    generation: u64,
    admission: Option<String>,
) {
    debug_assert!(agent.turn_lock.try_lock().is_err());
    // This turn reads `agent send` messages until its post-block seals.
    agent.mailbox.unseal();

    let turn_id = admission
        .as_ref()
        .map(|admission| TurnId::new(format!("trn_{admission}")))
        .unwrap_or_else(TurnId::generate);
    let cancel = CancellationToken::new();
    *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel.clone());
    if agent.turn_generation() != generation
        || agent.stop_requested()
        || admission
            .as_deref()
            .is_some_and(|admission| agent.admission_is_cancelled(admission))
    {
        cancel.cancel();
    }
    if agent.id().is_main() {
        let mut hbt = agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if hbt.is_none() {
            *hbt = Some(
                agent
                    .history
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            );
        }
    }

    tokio::spawn(async move {
        if admission.is_none() {
            match session
                .store
                .max_message_seq(&session.id, &agent.id())
                .await
            {
                Ok(Some(max_seq)) => {
                    let mut history = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                    history.next_seq = history.next_seq.max(max_seq.saturating_add(1));
                }
                Ok(None) => {}
                Err(error) => log::warn!(
                    "failed to synchronize history sequence for session={} agent={}: {error}",
                    session.id,
                    agent.id()
                ),
            }
        }
        if let Some(admission) = admission.as_deref() {
            let record = match claim_admission_for_turn(&session, &agent, admission).await {
                Ok(Some(record)) => record,
                Ok(None) => {
                    let _ = super::hide_other_admission_messages(&session, &agent, None).await;
                    unwind_unclaimed_turn(session, agent, turn_guard).await;
                    return;
                }
                Err(error) => {
                    park_admission_for_recovery(&session, &agent, generation, admission);
                    log::warn!("failed to claim admission {admission}: {error}");
                    unwind_unclaimed_turn(session, agent, turn_guard).await;
                    return;
                }
            };
            agent.commit_pending_user_messages();
            if let Err(error) = restore_admission_message(&session, &agent, &record).await {
                park_admission_for_recovery(&session, &agent, generation, admission);
                log::warn!("failed to restore admission {admission}: {error}");
                unwind_unclaimed_turn(session, agent, turn_guard).await;
                return;
            }
        }

        if cancel.is_cancelled()
            || agent.turn_generation() != generation
            || agent.stop_requested()
            || admission
                .as_deref()
                .is_some_and(|admission| agent.admission_is_cancelled(admission))
        {
            cancel.cancel();
        }

        if let Some(admission) = &admission {
            let _ = agent.begin_admission_turn(admission.clone(), turn_id.clone());
        }
        agent.set_current_turn(&turn_id);
        let spawned_at = std::time::Instant::now();
        agent.reset_turn_usage();

        // The turn runs on its own task; the outer task supervises: a panic or
        // abort inside `run_turn` lands here as a `JoinError` instead of silently
        // orphaning the agent — `cancel` stuck set, `is_running()` true forever,
        // no `TurnEnded`, no report to the parent.
        let turn_task = tokio::spawn({
            let session = session.clone();
            let agent = agent.clone();
            let turn_id = turn_id.clone();
            let admission = admission.clone();
            async move { super::run_turn(session, agent, cancel, turn_id, admission).await }
        });

        let (stop, returned_usage, crashed, preempted) = match turn_task.await {
            Ok(end) => (end.stop, Some(end.usage), false, end.preempted),
            Err(err) => (report_turn_crash(&session, &agent, &err), None, true, false),
        };
        let usage = {
            let recorded = agent.take_turn_usage();
            if recorded.is_empty() {
                returned_usage
            } else {
                Some(recorded)
            }
        };
        // The questions an orchestrator turn delivered end with it when the
        // turn ends on the model's terms (unanswered) or fails (the user is
        // not left with a subagent blocked on a turn that broke). A turn the
        // user interrupted, or a user message preempted, never had its say:
        // its questions stay open for the next one.
        if agent.id().is_main() {
            match &stop {
                TurnStop::Completed if !preempted => crate::rpc::resolve_delivered(
                    &session,
                    &agent,
                    pacode_tools::ParentReply::Unanswered,
                ),
                TurnStop::Failed { .. } => crate::rpc::resolve_delivered(
                    &session,
                    &agent,
                    pacode_tools::ParentReply::ParentFailed,
                ),
                TurnStop::Completed | TurnStop::Interrupted => {}
            }
        }
        let completed_admission = agent.finish_admission_turn(&turn_id, &stop);
        if let Some(admission) = completed_admission {
            if admission.terminal || agent.stop_requested() {
                let fenced = agent.admission_is_cancelled(&admission.message_id);
                if let Err(error) = session
                    .store
                    .mark_message_admission_terminal(
                        &session.id,
                        &admission.message_id,
                        Some(persisted_terminal_stop(fenced, &stop)),
                    )
                    .await
                {
                    log::warn!(
                        "failed to persist admission {}: {error}",
                        admission.message_id
                    );
                }
            } else {
                agent
                    .injections
                    .restore(vec![crate::inject::Injection::UserSteer {
                        admission: Some(admission.message_id),
                    }]);
                if crashed {
                    agent.clear_admission_turn(&turn_id);
                }
            }
        } else if let Some(admission) = admission {
            if agent.stop_requested() || agent.admission_is_cancelled(&admission) {
                if let Err(error) = session
                    .store
                    .mark_message_admission_terminal(&session.id, &admission, Some("interrupted"))
                    .await
                {
                    log::warn!("failed to persist admission {admission}: {error}");
                }
            } else if crashed {
                agent
                    .injections
                    .restore(vec![crate::inject::Injection::UserSteer {
                        admission: Some(admission.clone()),
                    }]);
            } else if let Err(error) = session
                .store
                .mark_message_admission_terminal(&session.id, &admission, Some("failed"))
                .await
            {
                log::warn!("failed to persist admission {admission}: {error}");
            }
        }
        agent.set_terminal_turn(&turn_id, &stop);

        // A turn that died mid-dispatch (panic/abort) can leave the persisted
        // assistant `tool_use`s unanswered — the next request would 400 on
        // every provider (`tool_use ids found without tool_result`). Stub them
        // before anyone observes the ended turn; an intact tail is a no-op.
        crate::turn::tools::stub_dangling_tool_calls(&session, &agent).await;

        if let Some(usage) = usage {
            let context_tokens = agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .estimated_tokens;
            let turn_number = session
                .usage
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .turns
                .saturating_add(1);
            if let Err(error) = session
                .record_usage(&agent.id(), &turn_id, turn_number, usage, context_tokens)
                .await
            {
                log::warn!("failed to persist turn usage: {error}");
            }
        }

        // Always cleared, whatever happened above — this is what was missing
        // when a panic left `cancel` set and the UI stuck on "running".
        *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;

        let recovery_generation = agent.turn_generation();
        let mut recovery_admission = if agent.stop_requested() {
            None
        } else {
            session.take_recovery_admission(&agent.id())
        };
        if let Some(admission) = recovery_admission.take() {
            agent
                .injections
                .restore(vec![crate::inject::Injection::UserSteer {
                    admission: Some(admission.clone()),
                }]);
            #[cfg(test)]
            wait_for_lifecycle_gate(&session, &agent).await;
            if agent.stop_requested() || agent.turn_generation() != recovery_generation {
                remove_user_admission(&agent, &admission);
                if let Err(error) = session
                    .store
                    .mark_message_admission_terminal(&session.id, &admission, Some("interrupted"))
                    .await
                {
                    log::warn!("failed to persist admission {admission}: {error}");
                }
            } else {
                recovery_admission = Some(admission);
            }
        }
        let has_recovery = recovery_admission.is_some();
        // Whether this turn owes the agent another one. Computed here for the
        // subagent-finalization branch below, and recomputed once the lock is
        // released — see the second call at the end of this task.
        // An `agent send` that reached a subagent's queue is owed a turn like a
        // user admission is: the orchestrator was told it was delivered.
        let owes_turn = |agent: &Agent, has_recovery: bool| {
            !agent.stop_requested()
                && !agent.injections.is_empty()
                && (agent.id().is_main()
                    || has_recovery
                    || agent.injections.has_user_admission()
                    || agent.injections.has_parent_message())
        };
        let mut should_redrive = owes_turn(&agent, has_recovery);
        // A subagent about to finish seals its mailbox first, under the lock
        // `agent send` decides under: a message that got in before the seal
        // re-drives this agent, one after it resumes the finished agent. Without
        // the seal a message landing here would be dropped with the history.
        if !agent.id().is_main()
            && !matches!(stop, TurnStop::Interrupted)
            && !agent.stop_requested()
            && !should_redrive
        {
            #[cfg(test)]
            crate::rpc::seal_hooks::run(&agent.id(), crate::rpc::seal_hooks::Point::BeforeSeal);
            should_redrive = !agent.mailbox.seal_unless_message(&agent.injections);
            #[cfg(test)]
            if !should_redrive {
                crate::rpc::seal_hooks::run(&agent.id(), crate::rpc::seal_hooks::Point::AfterSeal);
            }
        }

        // A stopped subagent keeps `Stopped` and its history so `continue` can
        // re-drive it later. No report is injected into the parent: it learned
        // about the stop as a tool result (or was interrupted itself), and
        // re-driving it here would defeat `InterruptAll`. The check is on this
        // turn's stop reason, not only the flag: `continue_agent` clears
        // `stop_requested` while an interrupted turn's post-block may still be
        // running — keying on `stop` keeps that unwinding turn from clobbering
        // the resumed agent (Finished status, parent injection, history drop).
        if !agent.id().is_main()
            && !matches!(stop, TurnStop::Interrupted)
            && !agent.stop_requested()
            && !should_redrive
        {
            // What lands on the agent card and what the parent is told come
            // from this one block: `Completed` earns the last assistant text
            // as a summary; `Failed` records the error instead of
            // masquerading as a ✓ — the report says so too.
            let answer = match &stop {
                TurnStop::Failed { message } => format!("subagent failed: {message}"),
                TurnStop::Completed | TurnStop::Interrupted => {
                    let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                    hist.messages
                        .iter()
                        .rev()
                        .find_map(|m| {
                            if m.role == Role::Assistant {
                                Some(m.text())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                }
            };
            let capped_answer = pacode_types::truncate_head_tail(
                &answer,
                session.config.context.injection_cap_chars,
            );

            {
                let mut info = agent.info.write().unwrap_or_else(|p| p.into_inner());
                match &stop {
                    TurnStop::Failed { message } => {
                        info.status = AgentStatus::Failed;
                        info.activity = Some("failed".to_string());
                        info.error = Some(message.clone());
                    }
                    TurnStop::Completed | TurnStop::Interrupted => {
                        info.status = AgentStatus::Finished;
                        info.activity = None;
                        info.summary = Some(capped_answer.clone());
                    }
                }
                if info.finished_at_ms.is_none() {
                    info.finished_at_ms = Some(pacode_types::now_ms());
                }
            }
            let info = agent.info();
            let _ = session
                .store
                .upsert_agent(&session.id, &info, agent.prompt().as_deref())
                .await;
            session.events.emit(Event::AgentUpdated(info.clone()));

            if let Some(parent_id) = &info.parent
                && let Some(parent) = session.agent(parent_id)
            {
                let injection = crate::inject::Injection::AgentFinished {
                    agent: info,
                    answer: capped_answer,
                };
                // A parked report starts no turn: the parent would spend a
                // model round trip reading an empty injection set.
                if !session.hold_notice(&parent, &injection) {
                    parent.injections.push(injection);
                    if parent.id().is_main() {
                        request_turn(session.clone(), parent);
                    }
                }
            }

            {
                let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                hist.clear();
            }
            #[cfg(test)]
            wait_for_lifecycle_gate(&session, &agent).await;
        }

        emit_turn_end(&session, &agent, &turn_id, spawned_at, usage, &stop).await;
        // Test-only: parks the turn inside the window between the redrive
        // decision above and the lock release below — exactly where an injector
        // can arrive and be told the agent is busy.
        #[cfg(test)]
        wait_for_lifecycle_gate(&session, &agent).await;
        drop(turn_guard);
        session.emit_session_status();

        // Re-checked *after* the lock is released, not reused from above.
        //
        // Between deciding `should_redrive` and dropping the guard, this turn
        // still owns the agent: an injector arriving in that window sees
        // `is_running() == true` and deliberately skips the wake, because a
        // running turn is supposed to pick the injection up itself. Trusting
        // the earlier decision strands it — the queue is non-empty, the agent
        // is idle, and nothing else will start a turn. That is a stuck session,
        // not a slow one, and it gets likelier exactly where this codebase aims
        // to work: 128 subagents reporting into a parent that is mid-teardown.
        //
        // With the lock dropped the race closes in both directions: an injector
        // either already woke the agent (this call finds the turn claimed and
        // returns without doing anything) or will do so itself.
        if owes_turn(&agent, has_recovery) {
            request_turn(session.clone(), agent.clone());
        } else if agent.notice_lock.wants_work() {
            spawn_locked_redrive(session.clone(), agent.clone());
        }
    });
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
