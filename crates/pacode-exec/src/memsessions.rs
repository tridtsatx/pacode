//! The set of sessions that are live *right now*, which is the divisor the
//! ceiling is split by.
//!
//! # How the count stays honest
//!
//! A division that is never given back is worse than no division: if a session
//! that ended still occupies its share, every later session runs at a fraction
//! of what the user configured and nothing ever fixes it. The mirror failure is
//! just as bad — a share taken back while somebody is still holding it, which
//! silently raises everyone else's limit above what they configured. Three
//! properties make both impossible here.
//!
//! 1. **The divisor is derived, not tracked.** There is no session counter to
//!    drift: the answer is the number of entries in a map, and an entry exists
//!    only while a lease holds it. What the map holds per session is the
//!    *depth* of that session's leases, which is not a second count of sessions
//!    — it exists only so the last holder, and not the first, ends the session.
//! 2. **Holding is by lease, not by call.** A session that opens gets a
//!    [`SessionLease`]; the depth drops when the lease is dropped, including on
//!    the unwind path of a panic and when the daemon's own session state is
//!    torn down. A caller therefore cannot forget to give the share back,
//!    because it never calls "close" — it just stops holding.
//! 3. **A crashed daemon cannot leave a stale division behind.** Nothing here
//!    is written to disk and nothing is restored at startup, so the state a
//!    crashed daemon held does not exist in the next process. The registry is
//!    fed by the daemon's own session lifecycle — the lease is parked in the
//!    session's state and dropped when the session is closed — so there is no
//!    second lifecycle to fall out of step with it.
//!
//! A session that reconnects gets a second lease, and a close that arrives
//! twice on a session that is already gone must not move the divisor. Both hold
//! because the depth is the unit: an explicit close releases one holder, and a
//! close of a session nobody holds removes nothing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pacode_types::SessionId;

/// Live sessions of one daemon process, each with the number of leases holding
/// it. Cloning shares the map, which is what lets a session hold its lease for
/// as long as the session is alive.
#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    live: Arc<Mutex<BTreeMap<SessionId, usize>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session as live and return the lease that keeps it counted.
    /// A second open of the same session returns a second lease but leaves the
    /// session counted once.
    pub fn open(&self, session: &SessionId) -> SessionLease {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        *live.entry(session.clone()).or_insert(0) += 1;
        SessionLease {
            registry: self.clone(),
            session: session.clone(),
        }
    }

    /// Release one holder of a session. The session stops being live when the
    /// last one lets go, not the first: a reconnecting client that holds two
    /// leases keeps its share for as long as either is alive. Unknown sessions
    /// are ignored, so a late or duplicated close cannot drive the divisor
    /// below the truth.
    pub fn close(&self, session: &SessionId) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let Some(depth) = live.get_mut(session) else {
            return;
        };
        *depth = depth.saturating_sub(1);
        if *depth == 0 {
            live.remove(session);
        }
    }

    /// The divisor: how many distinct sessions are live.
    pub fn live(&self) -> usize {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.len()
    }

    pub fn is_live(&self, session: &SessionId) -> bool {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.contains_key(session)
    }
}

/// Keeps a session counted for as long as it is held.
///
/// Owns a share of the registry rather than borrowing it, so the daemon can
/// park one in its session state and have it released by the ordinary drop of
/// that state. Not `Clone`: two copies would be two ways to give the same share
/// back twice.
#[derive(Debug)]
pub struct SessionLease {
    registry: SessionRegistry,
    session: SessionId,
}

impl SessionLease {
    pub fn session(&self) -> &SessionId {
        &self.session
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.registry.close(&self.session);
    }
}

#[cfg(test)]
#[path = "memsessions_tests.rs"]
mod memsessions_tests;
