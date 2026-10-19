//! Turn spawning and error notification lifecycle helpers.

mod admission;
mod ending;
#[cfg(test)]
mod gate;

use std::collections::HashSet;
use std::sync::Arc;

use pacode_types::{AgentStatus, Event, Role, TurnId, TurnStop};
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::notice_lock::{DueOrigin, Taken};
use crate::session::Session;
#[cfg(test)]
use admission::terminal_stop;
use admission::{
    claim_admission_for_turn, park_admission_for_recovery, persisted_terminal_stop,
    remove_user_admission, restore_admission_message, spawn_mark_admissions_terminal,
};
use ending::{emit_turn_end, report_turn_crash};
pub(crate) use ending::{finish_failed_turn, handle_interrupted};
#[cfg(test)]
use gate::{install_lifecycle_gate, wait_for_lifecycle_gate};

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

/// Shared exit for a turn that never reached `run_turn`: release the turn, and
/// start the next one when this turn left work behind.
async fn unwind_unclaimed_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    turn_guard: OwnedMutexGuard<()>,
) {
    *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let redrive = (!agent.injections.is_empty() || agent.notice_lock.wants_work())
        && !agent.stop_requested()
        && agent.may_start_own_turn();
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
    if admission.is_some() {
        crate::plan_reminder::on_user_turn(&agent);
    }

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
        // After a user interrupt only the user's own message is owed a turn.
        let owes_turn = |agent: &Agent, has_recovery: bool| {
            !agent.stop_requested()
                && !agent.injections.is_empty()
                && ((agent.id().is_main() && agent.may_start_own_turn())
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
            crate::rpc::hooks::run(&agent.id(), crate::rpc::hooks::Point::BeforeSeal);
            should_redrive = !agent.mailbox.seal_unless_message(&agent.injections);
            #[cfg(test)]
            if !should_redrive {
                crate::rpc::hooks::run(&agent.id(), crate::rpc::hooks::Point::AfterSeal);
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
            retire_agent(&agent);
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

/// Items a retired (finished) subagent keeps in its in-memory transcript tail.
const RETIRED_TAIL: usize = 16;

/// Free what a finished subagent no longer needs in memory: transcript tail capped at
/// `RETIRED_TAIL` (16) items and `tool_items` emptied. Older items are served from the
/// store by `GetHistory`; `AgentInfo` plus its summary are untouched. The tool registry
/// and the prompt are kept: an `agent send` may resume a finished subagent
/// (`rpc::resume` reloads its history with that prompt and runs it with those tools).
pub(crate) fn retire_agent(agent: &Agent) {
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retire(RETIRED_TAIL);
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
