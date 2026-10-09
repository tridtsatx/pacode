//! `/clear`: reset the conversation in place (`Request::ClearContext`).
//!
//! The session survives — id, name, cwd, route, effort, mode, zdr, the notice
//! lock, cron jobs, monitors and background tasks (processes are not context).
//! What goes is everything the model remembers or a client drew from it: the
//! main agent's history and transcript, every subagent, the plan and the
//! context meter.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;

use pacode_types::{AgentId, Event, Plan};
use tokio::sync::OwnedMutexGuard;

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
///    holds them in. Holding them is what "never under a live turn" means. A
///    turn that started after step 1 is cancelled again rather than waited
///    out (see [`lock_turns_interrupting`]), and the agent set is re-read
///    under the locks: one spawned in the gap would otherwise run unlocked.
/// 3. The store forgets first, memory second: a refused write leaves the
///    session as it was, and clients that never see `ContextCleared` keep
///    their view.
/// 4. The events go out while the locks are still held, so nothing from a
///    turn started after the clear reaches a client ahead of them.
pub async fn clear_context(session: &Arc<Session>) -> Result<(), CoreError> {
    for _ in 0..LOCK_ATTEMPTS {
        session.interrupt_all().await;
        let agents = resident_agents(session);
        let turns = lock_turns_interrupting(session, &agents).await;
        let submit = session.submit_lock.lock().await;
        if !same_agents(&agents, &resident_agents(session)) {
            continue;
        }
        let result = clear_locked(session).await;
        drop(submit);
        drop(turns);
        // Refused or not: an injection that arrived while the locks were held
        // found the turn claimed and was left for a turn that is not coming.
        // A no-op when nothing is due.
        if let Some(main) = session.main_agent() {
            crate::turn::lifecycle::request_turn(session.clone(), main);
        }
        return result;
    }
    Err(CoreError::Invalid(
        "the session kept starting agents during the clear; try again".to_string(),
    ))
}

/// [`crate::session::lock_turns`] — same order, so it cannot cross another
/// caller — except that a turn holding a lock is cancelled instead of being
/// waited out. `interrupt_all` swept the session a moment ago, so a holder
/// now is a turn that started since: a redrive, a monitor or task firing, a
/// second client. Waiting for it would make `/clear` wait a whole model turn.
///
/// The waiter is queued before the holder is cancelled. The mutex hands a
/// released lock to its first waiter, so no newer turn can claim it between
/// the cancellation and the wait; the wait is for an unwind, never a turn.
pub(crate) async fn lock_turns_interrupting(
    session: &Session,
    agents: &[Arc<Agent>],
) -> Vec<OwnedMutexGuard<()>> {
    let mut guards = Vec::with_capacity(agents.len());
    for agent in agents {
        let acquire = agent.turn_lock.clone().lock_owned();
        tokio::pin!(acquire);
        let guard = match futures::poll!(&mut acquire) {
            Poll::Ready(guard) => guard,
            Poll::Pending => {
                cancel_turn(session, agent).await;
                acquire.await
            }
        };
        guards.push(guard);
    }
    guards
}

async fn cancel_turn(session: &Session, agent: &Arc<Agent>) {
    if agent.id().is_main() {
        agent.invalidate_turn_generation();
        session.interrupt();
    } else if let Err(error) = session.stop_agent(&agent.id()).await {
        log::warn!(
            "clear: failed to stop agent {} in {}: {error}",
            agent.id(),
            session.id
        );
    }
}

/// The clear itself; the caller holds every turn lock and `submit_lock`.
async fn clear_locked(session: &Arc<Session>) -> Result<(), CoreError> {
    let main = session
        .main_agent()
        .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;
    // Whatever reached the main agent after `interrupt_all` swept it — a
    // message admitted in the gap, an injection queued — belongs to the
    // conversation being cleared.
    session.sweep_main_admissions(&main).await;

    // Every agent's message and row writes go through its
    // `message_write_lock` (`Session::publish_agent`, `stop_agent`), so
    // holding them all keeps a late write from landing after the purge and
    // bringing a dropped subagent back. Nobody else holds two of these locks,
    // so the order is free: main first, then the subagents being dropped.
    let mut write_guards = vec![main.message_write_lock.clone().lock_owned().await];
    for agent in resident_agents(session) {
        if !Arc::ptr_eq(&agent, &main) {
            write_guards.push(agent.message_write_lock.clone().lock_owned().await);
        }
    }
    if session.closed.load(Ordering::SeqCst) {
        return Err(CoreError::Invalid("session is closed".into()));
    }
    session.store.clear_context(&session.id).await?;

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
    let usage = {
        let mut usage = session
            .usage
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        usage.context_tokens = 0;
        usage.clone()
    };
    session.events.emit(Event::ContextCleared);
    // `ContextCleared` resets the clients that know it; the meter also rides
    // the event every client already folds (ACP's usage update among them).
    session.events.emit(Event::UsageUpdated(usage));
    // The sweeps dropped every parked notice without telling the badge; the
    // switch itself stays where the user put it.
    session.emit_notice_lock(&main, main.notice_lock.held());
    drop(write_guards);
    Ok(())
}
