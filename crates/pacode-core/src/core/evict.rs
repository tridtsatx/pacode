//! Idle-session eviction (P0-8): unload sessions nobody is using from daemon memory.
//!
//! Everything an evicted session held is already in the store (meta, plan, agents,
//! transcript, cron jobs), so `Attach::Resume` rebuilds it; agents come back as
//! finished. What is lost is only in-memory state: the file ledger, the efforts cache,
//! in-memory monitors (a session with a live monitor is never evicted) and per-agent
//! transcript tails.

use std::time::{Duration, Instant};

use pacode_types::SessionId;

use super::Core;

impl Core {
    /// Unload sessions that have been idle for at least `max_idle`; returns how many.
    ///
    /// A session is evicted only when ALL hold: no running turn, no live subagent and no
    /// running task (`Session::is_busy`); no event subscriber; no live monitor; no enabled
    /// cron job due within `max_idle`; `meta.updated_at_ms` older than `max_idle`; and
    /// this method has seen it in that state for `max_idle` across calls (the first call
    /// that sees it idle only starts the clock, so a session that was just resumed and
    /// is about to be subscribed is safe). Call it periodically (the daemon idle path
    /// runs it every 30 s with `max_idle = 10 min`).
    ///
    /// Eviction removes the session from `Core::sessions`, fires `session_end` hooks and
    /// drops per-session style-guard state; it does not touch the store and does not
    /// change agent statuses. Dropping the last `Arc<Session>` stops its scheduler.
    ///
    /// Cap of the bookkeeping: one `Instant` per live session, pruned on every call.
    /// Owner: `Core::idle_since`.
    pub fn evict_idle_sessions(&self, max_idle: Duration) -> usize {
        self.evict_idle_at(max_idle, Instant::now(), pacode_types::now_ms())
    }

    pub(crate) fn evict_idle_at(&self, max_idle: Duration, now: Instant, now_ms: u64) -> usize {
        let candidates: Vec<SessionId> = {
            let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
            let mut idle_since = self.idle_since.lock().unwrap_or_else(|p| p.into_inner());
            idle_since.retain(|id, _| sessions.contains_key(id));
            let mut ready = Vec::new();
            for (id, session) in sessions.iter() {
                if !is_idle(session, max_idle, now_ms) {
                    idle_since.remove(id);
                    continue;
                }
                let since = *idle_since.entry(id.clone()).or_insert(now);
                if now.saturating_duration_since(since) >= max_idle && since != now {
                    ready.push(id.clone());
                }
            }
            ready
        };

        let mut evicted = 0;
        for id in candidates {
            let removed = {
                let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
                // Re-check under the write lock: a client may have attached meanwhile.
                match sessions.get(&id) {
                    Some(session) if is_idle(session, max_idle, now_ms) => sessions.remove(&id),
                    _ => None,
                }
            };
            self.idle_since
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            if let Some(session) = removed {
                crate::hooks::session_end(&session);
                crate::style_guard::forget_session(&id);
                evicted += 1;
            }
        }
        evicted
    }
}

fn is_idle(session: &crate::session::Session, max_idle: Duration, now_ms: u64) -> bool {
    let max_idle_ms = u64::try_from(max_idle.as_millis()).unwrap_or(u64::MAX);
    if session.events.receiver_count() > 0 || session.is_busy() {
        return false;
    }
    if now_ms.saturating_sub(session.meta().updated_at_ms) < max_idle_ms {
        return false;
    }
    if !session.scheduler.monitors().is_empty() {
        return false;
    }
    let horizon = now_ms.saturating_add(max_idle_ms);
    !session
        .scheduler
        .jobs()
        .iter()
        .any(|job| job.enabled && job.next_run_ms.is_some_and(|next| next <= horizon))
}

#[cfg(test)]
#[path = "evict_tests.rs"]
mod evict_tests;
