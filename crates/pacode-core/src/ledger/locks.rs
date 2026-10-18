//! Per-path async write locks: writers of one path queue up, different paths proceed
//! in parallel.
//!
//! Cache: the lock map, owner = the session's `FileLedger`. An entry exists only
//! while a guard holds the path or a task waits for it: the last guard to drop
//! removes it (invalidation = idle). Cap: [`LOCK_MAP_CAP`]; entries left behind by
//! cancelled waiters are pruned when an insert finds the map at the cap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub const LOCK_MAP_CAP: usize = 256;

type LockMap = HashMap<PathBuf, Arc<AsyncMutex<()>>>;

#[derive(Default)]
pub struct PathLocks {
    map: Arc<Mutex<LockMap>>,
}

/// Holds one path until dropped.
pub struct PathGuard {
    key: PathBuf,
    map: Arc<Mutex<LockMap>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl PathLocks {
    fn entry(&self, key: &Path) -> Arc<AsyncMutex<()>> {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= LOCK_MAP_CAP && !map.contains_key(key) {
            // Only the map holds an idle entry.
            map.retain(|_, m| Arc::strong_count(m) > 1);
        }
        map.entry(key.to_path_buf())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Wait for `key` and hold it.
    pub async fn lock(&self, key: PathBuf) -> PathGuard {
        let mutex = self.entry(&key);
        let guard = mutex.lock_owned().await;
        PathGuard {
            key,
            map: self.map.clone(),
            guard: Some(guard),
        }
    }

    /// Hold `key` only if nobody holds it now.
    pub fn try_lock(&self, key: PathBuf) -> Option<PathGuard> {
        let mutex = self.entry(&key);
        let guard = mutex.try_lock_owned().ok();
        let held = PathGuard {
            key,
            map: self.map.clone(),
            guard,
        };
        // Dropping a guard without a lock still cleans the entry up.
        if held.guard.is_some() {
            Some(held)
        } else {
            None
        }
    }

    /// Paths currently in the map (held or awaited).
    pub fn count(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        // Release first, then forget the entry when nobody else holds or awaits it.
        drop(self.guard.take());
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map
            .get(&self.key)
            .is_some_and(|m| Arc::strong_count(m) == 1)
        {
            map.remove(&self.key);
        }
    }
}
