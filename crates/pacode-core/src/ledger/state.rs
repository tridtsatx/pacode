//! The pure ledger: per path, the current version, who wrote it, and what each agent
//! has seen. No I/O and no locking; [`super::FileLedger`] wraps it in a mutex and
//! feeds it file contents read under the path's write lock.
//!
//! Diffs are looked up in a shared [`DiffMemo`]: [`super::FileLedger`] computes the
//! diff a call will need before it takes the state lock (the `peek_*` methods hand it
//! the snapshots to diff), so under the lock a diff is normally a hit. A miss computes
//! inline, which is correct, only slower.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_types::AgentId;

use super::diff_memo::DiffMemo;
use super::entry::{PathEntry, Seen, unacked_changes};
use super::partial::{Partial, Resolution, same_stat};
use super::ranges::{LineRanges, overlaps};
use super::snapshot::{Snapshot, Stat};

pub use super::types::{DigestRow, Drift, Verdict, Writer};

/// Tracked paths per session; past it the least recently used path is forgotten
/// (with every agent's seen state for it, so "read first" applies to it again).
pub const MAX_PATHS: usize = 2048;

/// What a digest row needs from the state, cloned under the lock so the diff behind
/// its drift can run after the lock is released ([`Self::resolve`]).
pub struct DigestInput {
    path: PathBuf,
    wrote: bool,
    read: LineRanges,
    fresh_now: bool,
    base: Arc<Snapshot>,
    current: Arc<Snapshot>,
    writers: Vec<Writer>,
}

impl DigestInput {
    /// The finished row; diffs `base` against `current` (memoized).
    pub fn resolve(self, memo: &DiffMemo) -> DigestRow {
        let changes = unacked_changes(&self.base, &self.current, &self.read, self.fresh_now, memo);
        let drift = (!changes.is_empty()).then(|| Drift {
            writers: self.writers,
            changes,
            line_count: self.current.line_count(),
            coarse: self.base.whole_only || self.current.whole_only,
        });
        DigestRow {
            path: self.path,
            version: self.current.short(),
            wrote: self.wrote,
            read: self.read,
            drift,
        }
    }
}

/// All tracked paths of one session. Cache: owner = the session's `FileLedger`; cap
/// [`MAX_PATHS`] paths with LRU eviction (an access = any read, check, write or
/// refresh of the path); invalidation = content hash mismatch on every access.
pub struct LedgerState {
    entries: HashMap<PathBuf, PathEntry>,
    tick: u64,
    cap: usize,
    memo: Arc<DiffMemo>,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self::with_cap(MAX_PATHS)
    }
}

impl LedgerState {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
            cap: cap.max(1),
            memo: Arc::new(DiffMemo::default()),
        }
    }

    /// The diff memo this state reads (shared with the owning `FileLedger`).
    pub fn memo(&self) -> Arc<DiffMemo> {
        self.memo.clone()
    }

    pub fn tracked_count(&self) -> usize {
        self.entries.len()
    }

    pub fn is_tracked(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    /// `agent`'s base version of `path`: the older side of the diff its next
    /// check/read/ack will need.
    pub fn peek_base(&self, agent: &AgentId, path: &Path) -> Option<Arc<Snapshot>> {
        Some(self.entries.get(path)?.seen.get(agent)?.base.clone())
    }

    /// The path's current version: the older side of the diff a write will need.
    pub fn peek_current(&self, path: &Path) -> Option<Arc<Snapshot>> {
        Some(self.entries.get(path)?.current.clone())
    }

    /// `agent`'s deferred ranged read of `path`, if any.
    pub fn peek_partial(&self, agent: &AgentId, path: &Path) -> Option<Arc<Partial>> {
        self.entries.get(path)?.seen.get(agent)?.partial.clone()
    }

    fn bump(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Make room for one more path by dropping the least recently used ones.
    fn evict_for_insert(&mut self) {
        while self.entries.len() >= self.cap {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(p, _)| p.clone());
            match oldest {
                Some(p) => {
                    self.entries.remove(&p);
                }
                None => break,
            }
        }
    }

    /// The entry for `path`, created when untracked and advanced to `snap` (written
    /// by `writer`) when its content differs from the last known version.
    fn sync(
        &mut self,
        path: &Path,
        snap: Arc<Snapshot>,
        stat: Stat,
        writer: Writer,
    ) -> (&mut PathEntry, u64) {
        let tick = self.bump();
        if !self.entries.contains_key(path) {
            self.evict_for_insert();
        }
        let entry = self
            .entries
            .entry(path.to_path_buf())
            .or_insert_with(|| PathEntry::new(snap.clone(), stat, tick));
        entry.advance(snap, stat, writer);
        entry.last_used = tick;
        (entry, tick)
    }

    /// `agent` read `snap` and was shown `lines`. A read that covers every change it
    /// had not seen (or the whole file) brings it in sync with the current version.
    pub fn record_read(
        &mut self,
        agent: &AgentId,
        path: &Path,
        snap: Arc<Snapshot>,
        stat: Stat,
        lines: Range<usize>,
    ) {
        let memo = self.memo.clone();
        let (entry, tick) = self.sync(path, snap, stat, Writer::External);
        let current = entry.current.clone();
        let whole = lines.start == 0 && lines.end >= current.line_count();
        let mut seen = entry
            .seen
            .remove(agent)
            .unwrap_or_else(|| Seen::at_version(current.clone(), tick));
        if seen.fresh_hash != current.hash {
            seen.fresh.clear();
            seen.fresh_hash = current.hash;
        }
        seen.fresh.insert(lines);
        seen.wrote = false;
        seen.at = tick;
        seen.partial = None;
        if whole || entry.unacked(&seen, &memo).is_empty() {
            seen.base = current;
        }
        entry.seen.insert(agent.clone(), seen);
    }

    /// `agent` was shown lines `first..first + hashes.len()` of a file too large to
    /// snapshot (a streamed range), at file stat `stat`; `hashes` are
    /// [`super::snapshot::hash_line`] of each. No whole-file hash exists: when the
    /// path's last known version has the same len/mtime the read is an ordinary
    /// ranged read of it, otherwise it is kept as a [`Partial`] that the next write
    /// check resolves against the full text.
    pub fn record_read_range(
        &mut self,
        agent: &AgentId,
        path: &Path,
        stat: Stat,
        first: usize,
        hashes: Vec<u64>,
    ) {
        let memo = self.memo.clone();
        let tick = self.bump();
        if !self.entries.contains_key(path) {
            self.evict_for_insert();
        }
        let entry = self
            .entries
            .entry(path.to_path_buf())
            .or_insert_with(|| PathEntry::pending(stat, tick));
        entry.last_used = tick;
        let current = entry.current.clone();
        let mut seen = entry
            .seen
            .remove(agent)
            .unwrap_or_else(|| Seen::at_version(current.clone(), tick));
        seen.wrote = false;
        seen.at = tick;
        if !entry.pending && same_stat(&entry.stat, &stat) {
            // The known version is the file as it is now.
            if seen.fresh_hash != current.hash {
                seen.fresh.clear();
                seen.fresh_hash = current.hash;
            }
            seen.fresh.insert(first..first + hashes.len());
            seen.partial = None;
            if entry.unacked(&seen, &memo).is_empty() {
                seen.base = current;
            }
        } else {
            let merged = Partial::merged(seen.partial.as_deref(), stat, first, hashes);
            seen.partial = Some(Arc::new(merged));
        }
        entry.seen.insert(agent.clone(), seen);
    }

    /// Settle `agent`'s deferred ranged read of `path` against the file as it is now
    /// (`snap`/`stat`, full text hashed by the caller). The entry moves to `snap`.
    /// `Unchanged` brings the agent in sync (its seen lines become its fresh
    /// ranges); returns whether the caller may pass the write without a verdict
    /// (`Covers`: the write touches only lines the agent saw).
    pub fn apply_partial(
        &mut self,
        agent: &AgentId,
        path: &Path,
        snap: Arc<Snapshot>,
        stat: Stat,
        resolution: Resolution,
    ) -> bool {
        let (entry, tick) = self.sync(path, snap, stat, Writer::External);
        match resolution {
            Resolution::Covers => true,
            Resolution::Stale => false,
            Resolution::Unchanged => {
                let current = entry.current.clone();
                if let Some(seen) = entry.seen.get_mut(agent)
                    && let Some(partial) = seen.partial.take()
                {
                    seen.base = current;
                    seen.fresh_hash = seen.base.hash;
                    seen.fresh = partial.ranges();
                    seen.at = tick;
                }
                false
            }
        }
    }

    /// May `agent` write over `current` (`None` = the file does not exist)? `touched`
    /// = the current lines the write changes; `None` = the edit could not be applied,
    /// so any unseen change counts as the likely cause.
    pub fn check(
        &mut self,
        agent: &AgentId,
        path: &Path,
        current: Option<(Arc<Snapshot>, Stat)>,
        touched: Option<&[Range<usize>]>,
    ) -> Verdict {
        let Some((snap, stat)) = current else {
            return Verdict::Clean;
        };
        let memo = self.memo.clone();
        // A content mismatch with the last known version is an external change.
        let (entry, _) = self.sync(path, snap, stat, Writer::External);
        let Some(seen) = entry.seen.get(agent).cloned() else {
            return Verdict::NotRead;
        };
        let changes = entry.unacked(&seen, &memo);
        if changes.is_empty() {
            let cur = entry.current.clone();
            if let Some(s) = entry.seen.get_mut(agent) {
                s.base = cur;
            }
            return Verdict::Clean;
        }
        let conflict = match touched {
            None => true,
            Some(t) => changes
                .iter()
                .any(|c| t.iter().any(|r| overlaps(&c.new, r))),
        };
        let drift = entry.drift(&seen, changes);
        if conflict {
            Verdict::Conflict(drift)
        } else {
            Verdict::Disjoint(drift)
        }
    }

    /// `agent` was shown the current `lines` of `path` (e.g. in a rejection): they
    /// count as re-read.
    pub fn ack(&mut self, agent: &AgentId, path: &Path, lines: &[Range<usize>]) {
        let memo = self.memo.clone();
        let tick = self.bump();
        let Some(entry) = self.entries.get_mut(path) else {
            return;
        };
        let Some(mut seen) = entry.seen.remove(agent) else {
            return;
        };
        if seen.fresh_hash != entry.current.hash {
            seen.fresh.clear();
            seen.fresh_hash = entry.current.hash;
        }
        for r in lines {
            seen.fresh.insert(r.clone());
        }
        seen.at = tick;
        if entry.unacked(&seen, &memo).is_empty() {
            seen.base = entry.current.clone();
        }
        entry.seen.insert(agent.clone(), seen);
    }

    /// `agent` (display `name`) wrote `snap`. Its own view follows its write; other
    /// agents keep their older base and see the change as drift.
    pub fn record_write(
        &mut self,
        agent: &AgentId,
        name: &str,
        path: &Path,
        snap: Arc<Snapshot>,
        stat: Stat,
    ) {
        let memo = self.memo.clone();
        let writer = Writer::agent(agent, name);
        let created = !self.entries.contains_key(path);
        let before = self.entries.get(path).map(|e| e.current.clone());
        let (entry, tick) = self.sync(path, snap.clone(), stat, writer.clone());
        let mut fresh = LineRanges::default();
        match before {
            Some(prev) => {
                for c in memo.diff(&prev, &snap).iter() {
                    fresh.insert(c.new.clone());
                }
            }
            None => fresh.insert(0..snap.line_count()),
        }
        if created {
            entry.last_writer = Some(writer);
        }
        entry.seen.insert(
            agent.clone(),
            Seen {
                fresh_hash: snap.hash,
                base: snap,
                fresh,
                wrote: true,
                at: tick,
                partial: None,
            },
        );
    }

    /// A command run by `agent` left `path` at `snap`. Only tracked paths are
    /// refreshed. The change is attributed to `agent`, whose view follows it when it
    /// was in sync before (it caused the change); other agents see drift.
    pub fn refresh(
        &mut self,
        agent: &AgentId,
        name: &str,
        path: &Path,
        snap: Arc<Snapshot>,
        stat: Stat,
    ) -> bool {
        let tick = self.bump();
        let Some(entry) = self.entries.get_mut(path) else {
            return false;
        };
        entry.last_used = tick;
        if snap.hash == entry.current.hash {
            entry.stat = stat;
            return false;
        }
        let was_in_sync = entry
            .seen
            .get(agent)
            .is_some_and(|s| s.base.hash == entry.current.hash);
        entry.advance(snap.clone(), stat, Writer::agent(agent, name));
        if was_in_sync && let Some(seen) = entry.seen.get_mut(agent) {
            seen.fresh_hash = snap.hash;
            seen.fresh.clear();
            seen.base = snap;
            seen.at = tick;
        }
        true
    }

    /// Paths a command run by `agent` may have changed, with their last known stat:
    /// those `agent` has seen plus those any agent wrote through pacode, most recently
    /// used first, at most `limit`. Paths only other agents read are left out (a
    /// change there surfaces as external at their next check).
    pub fn refresh_candidates(&self, agent: &AgentId, limit: usize) -> Vec<(PathBuf, Stat)> {
        let mut rows: Vec<(u64, &PathBuf, Stat)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.seen.contains_key(agent) || e.last_writer.is_some())
            .map(|(p, e)| (e.last_used, p, e.stat))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        rows.truncate(limit);
        rows.into_iter().map(|(_, p, s)| (p.clone(), s)).collect()
    }

    /// Give `to` the same seen state as `from` (an agent changing identity).
    pub fn copy_agent(&mut self, from: &AgentId, to: &AgentId) {
        for entry in self.entries.values_mut() {
            if let Some(seen) = entry.seen.get(from).cloned() {
                entry.seen.insert(to.clone(), seen);
            }
        }
    }

    /// The paths `agent` has seen, most recent first, at most `limit` (only those get
    /// their drift computed), plus the number left out. The rows still need
    /// [`DigestInput::resolve`], which runs the diffs: call it after unlocking.
    pub fn digest_inputs(&self, agent: &AgentId, limit: usize) -> (Vec<DigestInput>, usize) {
        let mut rows: Vec<(u64, &PathBuf, &PathEntry, &Seen)> = self
            .entries
            .iter()
            .filter_map(|(p, e)| e.seen.get(agent).map(|s| (s.at, p, e, s)))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let omitted = rows.len().saturating_sub(limit);
        let inputs = rows
            .into_iter()
            .take(limit)
            .map(|(_, path, entry, seen)| {
                let fresh_now = seen.fresh_hash == entry.current.hash;
                let writers = if seen.base.hash == entry.current.hash {
                    Vec::new()
                } else {
                    entry.writers_since(seen.base.hash)
                };
                DigestInput {
                    path: path.clone(),
                    wrote: seen.wrote,
                    read: if fresh_now {
                        seen.fresh.clone()
                    } else {
                        LineRanges::default()
                    },
                    fresh_now,
                    base: seen.base.clone(),
                    current: entry.current.clone(),
                    writers,
                }
            })
            .collect();
        (inputs, omitted)
    }
}
