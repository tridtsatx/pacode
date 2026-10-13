//! How a turn ends: the resting status of an interrupted or failed turn, the
//! failure notice, a crashed turn task, and the `TurnEnded` tail the
//! supervisor emits once the cancel token is cleared.

use std::sync::Arc;

use pacode_types::{AgentStatus, Event, TurnId, TurnStop, Usage};

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
pub(super) async fn emit_turn_end(
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
pub(super) fn report_turn_crash(
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
