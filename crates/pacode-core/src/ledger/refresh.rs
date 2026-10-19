//! Bookkeeping for the cheap post-`bash` refresh ([`super::FileLedger::refresh_after_exec`]).
//!
//! One refresh = one `spawn_blocking` that stats at most [`REFRESH_MAX_PATHS`]
//! candidate paths in a loop ([`changed_paths`]); only paths whose len/mtime moved are
//! rehashed. Concurrent refreshes by one agent coalesce through [`RefreshGate`].
//!
//! Cache: the gate's per-agent "covered" map, owner = the session's `FileLedger`; cap
//! [`GATE_AGENT_CAP`] agents (cleared when full, which only loses coalescing);
//! invalidation = a newer sweep for the same agent overwrites its entry.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use pacode_types::AgentId;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};

use super::snapshot::Stat;

/// Paths one refresh may stat: the most recently used ones that the calling agent has
/// seen or that any agent wrote through pacode. Others are left to the next
/// `check`, which reports their change as external.
pub const REFRESH_MAX_PATHS: usize = 256;

/// Agents remembered by the coalescing map.
const GATE_AGENT_CAP: usize = 256;

#[derive(Default)]
struct Requests {
    /// Refresh requests made so far.
    next: u64,
    /// Per agent: the request count when its latest sweep started. A request numbered
    /// at or below it is already covered by that sweep.
    covered: HashMap<AgentId, u64>,
}

/// Serializes sweeps and lets a refresh that an earlier sweep of the same agent
/// already covers return without stat-ing again. Attribution stays per agent: a
/// sweep never absorbs another agent's request.
#[derive(Default)]
pub struct RefreshGate {
    sweep: AsyncMutex<()>,
    requests: Mutex<Requests>,
    sweeps: AtomicU64,
    rehashes: AtomicU64,
}

impl RefreshGate {
    /// Register a refresh request; the number orders it against sweeps.
    pub fn request(&self) -> u64 {
        let mut r = self.requests.lock().unwrap_or_else(|p| p.into_inner());
        r.next += 1;
        r.next
    }

    /// Wait for the running sweep, then start one for `agent`, unless a sweep of
    /// `agent` that began after request `want` already ran (`None`).
    pub async fn enter(&self, agent: &AgentId, want: u64) -> Option<MutexGuard<'_, ()>> {
        let guard = self.sweep.lock().await;
        {
            let mut r = self.requests.lock().unwrap_or_else(|p| p.into_inner());
            if r.covered.get(agent).is_some_and(|&c| c >= want) {
                return None;
            }
            if r.covered.len() >= GATE_AGENT_CAP {
                r.covered.clear();
            }
            let started = r.next;
            r.covered.insert(agent.clone(), started);
        }
        self.sweeps.fetch_add(1, Ordering::Relaxed);
        Some(guard)
    }

    pub fn count_rehash(&self) {
        self.rehashes.fetch_add(1, Ordering::Relaxed);
    }

    /// Stat sweeps started (tests).
    #[cfg(test)]
    pub fn sweeps(&self) -> u64 {
        self.sweeps.load(Ordering::Relaxed)
    }

    /// Files read and hashed by refreshes (tests).
    #[cfg(test)]
    pub fn rehashes(&self) -> u64 {
        self.rehashes.load(Ordering::Relaxed)
    }
}

/// The candidates whose current len/mtime differ from the known stat. Blocking: run
/// it inside one `spawn_blocking`. Missing paths are skipped.
pub fn changed_paths(candidates: Vec<(PathBuf, Stat)>) -> Vec<PathBuf> {
    candidates
        .into_iter()
        .filter_map(|(path, known)| {
            let meta = std::fs::metadata(&path).ok()?;
            let now = Stat {
                len: meta.len(),
                mtime: meta.modified().ok(),
            };
            (now != known).then_some(path)
        })
        .collect()
}
