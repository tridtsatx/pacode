//! Turn spawning and error notification lifecycle helpers.

use std::sync::Arc;

use pacode_types::{AgentStatus, Event, Role, TurnId, TurnStop, Usage};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;

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
            session.events.emit(Event::AgentUpdated(info.clone()));
            let session = session.clone();
            let agent = agent.clone();
            tokio::spawn(async move {
                let _ = session
                    .store
                    .upsert_agent(&session.id, &info, agent.prompt().as_deref())
                    .await;
            });
        }
        return;
    }

    // One turn at a time, claimed BEFORE the cancel token is installed: a
    // racing `start_turn` must not clobber the running turn's token (the live
    // turn would go invisible to `is_running` and uncancellable) nor run the
    // post-block over the winner's state. The guard is owned by the supervisor
    // task and held until the turn's token is cleared.
    let Ok(turn_guard) = agent.turn_lock.clone().try_lock_owned() else {
        return;
    };

    let cancel = CancellationToken::new();
    *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel.clone());
    // A `stop_agent`/`close_session` landing between the check above and the
    // token install would find nothing to cancel — the flag survives, so
    // re-check and self-cancel: the turn unwinds through the Interrupted path.
    if agent.stop_requested() {
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

    let turn_id = TurnId::generate();
    let spawned_at = std::time::Instant::now();

    // The turn runs on its own task; the outer task supervises: a panic or
    // abort inside `run_turn` lands here as a `JoinError` instead of silently
    // orphaning the agent — `cancel` stuck set, `is_running()` true forever,
    // no `TurnEnded`, no report to the parent.
    let turn_task = tokio::spawn({
        let session = session.clone();
        let agent = agent.clone();
        let turn_id = turn_id.clone();
        async move { super::run_turn(session, agent, cancel, turn_id).await }
    });

    tokio::spawn(async move {
        let (stop, usage) = match turn_task.await {
            Ok((stop, usage)) => (stop, Some(usage)),
            Err(err) => (report_turn_crash(&session, &agent, &err), None),
        };

        // A turn that died mid-dispatch (panic/abort) can leave the persisted
        // assistant `tool_use`s unanswered — the next request would 400 on
        // every provider (`tool_use ids found without tool_result`). Stub them
        // before anyone observes the ended turn; an intact tail is a no-op.
        crate::turn::tools::stub_dangling_tool_calls(&session, &agent).await;

        // Always cleared, whatever happened above — this is what was missing
        // when a panic left `cancel` set and the UI stuck on "running".
        *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;

        // `TurnEnded` goes out only after the token is cleared, and while the
        // guard is still held (`is_running` still true): a client reacting to
        // it can never observe a turn-ended-but-still-running gap, and a fresh
        // `start_turn` cannot emit `TurnStarted` ahead of this `TurnEnded`.
        emit_turn_end(&session, &agent, &turn_id, spawned_at, usage, &stop).await;
        drop(turn_guard);
        // Now that the guard is gone the badge reads `Done` (or `Working` when
        // another agent still runs; a queued injection re-drive flips it right
        // back via that turn's `TurnStarted`).
        session.emit_session_status();

        // An injection that landed between the last drain and the end of the turn
        // would otherwise wait for the next user message: run another turn for it.
        // The stop reason does not matter — a prompt arriving while Ctrl+C or a
        // provider failure unwound the turn is queued exactly the same way and
        // must not be orphaned. `stop_requested` (stop_agent / interrupt_all /
        // close_session) still wins. Bounded: each re-drive needs a queued
        // injection, so nothing loops without new input.
        if agent.id().is_main() && !agent.stop_requested() && !agent.injections.is_empty() {
            start_turn(session.clone(), agent.clone());
            return;
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
                parent
                    .injections
                    .push(crate::inject::Injection::AgentFinished {
                        agent: info,
                        answer: capped_answer,
                    });
                if parent.id().is_main() && !parent.is_running() {
                    start_turn(session.clone(), parent);
                }
            }

            {
                let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                hist.messages.clear();
            }
        }
    });
}
