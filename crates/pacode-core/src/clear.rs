//! `/clear`: reset the conversation in place (`Request::ClearContext`).
//!
//! The session survives — id, name, cwd, route, effort, mode, zdr, the notice
//! lock, cron jobs, monitors and background tasks (processes are not context).
//! What goes is everything the model remembers or a client drew from it: the
//! main agent's history and transcript, every subagent, the plan and the
//! context meter.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pacode_types::{AgentId, Event, Plan};

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

/// How many times the lock set is re-taken when an agent appeared or was
/// swapped while it was being taken. Every attempt starts with
/// `interrupt_all`, so a second change needs a fresh main turn to start and
/// spawn inside a window of a few awaits; past this the clear is refused
/// instead of looping.
const LOCK_ATTEMPTS: usize = 3;

fn resident_agents(session: &Session) -> Vec<Arc<Agent>> {
    session
        .agents
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect()
}

fn same_agents(locked: &[Arc<Agent>], now: &[Arc<Agent>]) -> bool {
    locked.len() == now.len()
        && locked
            .iter()
            .zip(now)
            .all(|(locked, now)| Arc::ptr_eq(locked, now))
}

/// Wipe the session's conversation and keep the session.
///
/// 1. `interrupt_all` — the main turn is cancelled and every live subagent
///    stopped, the path `Request::InterruptAll` takes.
/// 2. Every agent's turn lock, then `submit_lock`: the order a running turn
///    holds them in. Holding them is what "never under a live turn" means — a
///    turn that slipped in between 1 and 2 is waited out by its own
///    supervision, not cleared under. The agent set is re-read under the
///    locks; one spawned in the gap would otherwise keep running unlocked.
/// 3. The store forgets first, memory second: a refused write leaves the
///    session as it was, and clients that never see `ContextCleared` keep
///    their view.
/// 4. `ContextCleared` is emitted while the locks are still held, so no event
///    of a turn started after the clear can reach a client ahead of it.
pub async fn clear_context(session: &Arc<Session>) -> Result<(), CoreError> {
    for _ in 0..LOCK_ATTEMPTS {
        session.interrupt_all().await;
        let agents = resident_agents(session);
        let turns = crate::session::lock_turns(&agents).await;
        let submit = session.submit_lock.lock().await;
        if !same_agents(&agents, &resident_agents(session)) {
            continue;
        }
        let main = clear_locked(session).await?;
        drop(submit);
        drop(turns);
        // An injection that arrived while the locks were held found the turn
        // claimed and was left for a turn that is not coming. A no-op when
        // nothing is due.
        crate::turn::lifecycle::request_turn(session.clone(), main);
        return Ok(());
    }
    Err(CoreError::Invalid(
        "the session kept starting agents during the clear; try again".to_string(),
    ))
}

/// The clear itself; the caller holds every turn lock and `submit_lock`.
async fn clear_locked(session: &Arc<Session>) -> Result<Arc<Agent>, CoreError> {
    if session.closed.load(Ordering::SeqCst) {
        return Err(CoreError::Invalid("session is closed".into()));
    }
    let main = session
        .main_agent()
        .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;
    // Whatever reached the main agent after `interrupt_all` swept it — a
    // message admitted in the gap, an injection queued — belongs to the
    // conversation being cleared.
    session.sweep_main_admissions(&main).await;

    let _message_guard = main.message_write_lock.clone().lock_owned().await;
    let next_seq = main
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_seq;
    session.store.clear_context(&session.id, next_seq).await?;

    session
        .agents
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|id, _| id.is_main());
    // Aliases only ever name detached turns, which are subagents.
    session
        .agent_aliases
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    // Before the wipe, as in compaction: a checkpoint open across this line
    // lost the context it was asked in.
    main.note_compacted();
    // `History::clear` keeps `next_seq`, so the live seq space stays monotonic.
    main.history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    *main
        .history_before_turn
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    main.transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    main.seed_history_floor(None);
    *session
        .plan
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Plan::default();
    // The cost and token totals are what the session spent and stay; the
    // context meter describes what the model holds now, which is nothing.
    session
        .usage
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .context_tokens = 0;
    session.events.emit(Event::ContextCleared);
    // The sweeps dropped every parked notice without telling the badge; the
    // switch itself stays where the user put it.
    session.emit_notice_lock(&main, main.notice_lock.held());
    Ok(main)
}
