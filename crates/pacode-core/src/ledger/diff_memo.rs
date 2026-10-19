//! Memo of line diffs between two snapshots, so the ledger's state mutex is never held
//! across a diff (G4).
//!
//! A diff is a pure function of the two snapshots' content hashes, so a memoized entry
//! can never be stale: callers compute the diff they are about to need BEFORE taking
//! the state lock ([`super::FileLedger`] does, via `spawn_blocking` for big files),
//! store it here, and the locked section finds it with a lookup. A miss (another
//! writer moved the path in between) falls back to computing inline, which is correct,
//! only slower.
//!
//! Cache: key = `(old.hash, new.hash)`. Cap: [`DIFF_MEMO_CAP`] entries, oldest
//! inserted dropped first. Invalidation: none needed (content-addressed). Owner: the
//! session's `FileLedger`, shared with its `LedgerState`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::snapshot::{Change, Snapshot, diff_snapshots};

/// Diffs remembered.
pub const DIFF_MEMO_CAP: usize = 64;

/// Snapshots with more lines than this are diffed on a blocking thread.
pub const INLINE_DIFF_LINES: usize = 512;

#[derive(Default)]
struct Inner {
    map: HashMap<(u64, u64), Arc<Vec<Change>>>,
    order: VecDeque<(u64, u64)>,
}

#[derive(Default)]
pub struct DiffMemo {
    inner: Mutex<Inner>,
    computed: AtomicU64,
}

impl DiffMemo {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, old: &Snapshot, new: &Snapshot) -> Option<Arc<Vec<Change>>> {
        self.lock().map.get(&(old.hash, new.hash)).cloned()
    }

    /// The changes from `old` to `new`: memoized, else computed (without holding the
    /// memo's lock) and remembered.
    pub fn diff(&self, old: &Snapshot, new: &Snapshot) -> Arc<Vec<Change>> {
        if let Some(hit) = self.get(old, new) {
            return hit;
        }
        let changes = Arc::new(diff_snapshots(old, new));
        self.computed.fetch_add(1, Ordering::Relaxed);
        let key = (old.hash, new.hash);
        let mut inner = self.lock();
        if inner.map.insert(key, changes.clone()).is_none() {
            inner.order.push_back(key);
            while inner.order.len() > DIFF_MEMO_CAP {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.map.remove(&oldest);
                }
            }
        }
        changes
    }

    /// Whether diffing these two is worth a blocking thread.
    pub fn is_heavy(old: &Snapshot, new: &Snapshot) -> bool {
        old.lines.len().max(new.lines.len()) > INLINE_DIFF_LINES
    }

    /// Diffs actually computed so far (tests).
    #[cfg(test)]
    pub fn computed(&self) -> u64 {
        self.computed.load(Ordering::Relaxed)
    }

    /// Entries held (tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().map.len()
    }
}
