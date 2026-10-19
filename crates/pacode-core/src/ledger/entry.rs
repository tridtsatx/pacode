//! Per-path and per-agent records inside [`super::state::LedgerState`]: the path's
//! current version and write log, and what each agent has seen of it.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use pacode_types::AgentId;

use super::diff_memo::DiffMemo;
use super::partial::Partial;
use super::ranges::{LineRanges, visible};
use super::snapshot::{Change, Snapshot, Stat};
use super::types::{Drift, Writer};

/// Versions remembered per path to attribute changes to their writers.
pub(super) const MAX_LOG: usize = 32;

pub(super) struct LogEntry {
    pub to_hash: u64,
    pub writer: Writer,
}

/// One agent's view of one path.
#[derive(Clone)]
pub(super) struct Seen {
    /// The version the agent is in sync with: changes after it are drift.
    pub base: Arc<Snapshot>,
    /// Lines read (or shown in a rejection) at version `fresh_hash`.
    pub fresh: LineRanges,
    pub fresh_hash: u64,
    /// Its last action on the path was a write.
    pub wrote: bool,
    /// Ledger tick of its last action on the path (digest order).
    pub at: u64,
    /// Ranged read of a large file whose whole-file hash is deferred to the next write
    /// check (see [`super::partial`]).
    pub partial: Option<Arc<Partial>>,
}

impl Seen {
    pub fn at_version(snap: Arc<Snapshot>, tick: u64) -> Self {
        Self {
            fresh_hash: snap.hash,
            base: snap,
            fresh: LineRanges::default(),
            wrote: false,
            at: tick,
            partial: None,
        }
    }
}

pub(super) struct PathEntry {
    pub current: Arc<Snapshot>,
    pub stat: Stat,
    pub last_writer: Option<Writer>,
    pub log: VecDeque<LogEntry>,
    pub seen: HashMap<AgentId, Seen>,
    pub last_used: u64,
    /// `current` is a placeholder: the path was only seen through ranged reads of a
    /// large file, so no whole-file hash exists yet. The next full snapshot replaces
    /// it without being logged as a change.
    pub pending: bool,
}

impl PathEntry {
    pub fn new(current: Arc<Snapshot>, stat: Stat, tick: u64) -> Self {
        Self {
            current,
            stat,
            last_writer: None,
            log: VecDeque::new(),
            seen: HashMap::new(),
            last_used: tick,
            pending: false,
        }
    }

    /// An entry whose content was never hashed (ranged read of a large file).
    pub fn pending(stat: Stat, tick: u64) -> Self {
        let mut entry = Self::new(Snapshot::unknown(), stat, tick);
        entry.pending = true;
        entry
    }

    /// Move to `next` written by `writer`; only the stat changes when the content is
    /// the same.
    pub fn advance(&mut self, next: Arc<Snapshot>, stat: Stat, writer: Writer) {
        self.stat = stat;
        if self.pending {
            self.pending = false;
            self.current = next;
            self.last_writer = Some(writer);
            return;
        }
        if next.hash == self.current.hash {
            return;
        }
        self.current = next;
        self.log.push_back(LogEntry {
            to_hash: self.current.hash,
            writer: writer.clone(),
        });
        while self.log.len() > MAX_LOG {
            self.log.pop_front();
        }
        self.last_writer = Some(writer);
    }

    /// Distinct writers of the versions after `base_hash`, oldest first.
    pub fn writers_since(&self, base_hash: u64) -> Vec<Writer> {
        let start = self
            .log
            .iter()
            .rposition(|e| e.to_hash == base_hash)
            .map_or(0, |i| i + 1);
        let mut out: Vec<Writer> = Vec::new();
        for e in self.log.iter().skip(start) {
            if !out.contains(&e.writer) {
                out.push(e.writer.clone());
            }
        }
        if out.is_empty() {
            out.push(self.last_writer.clone().unwrap_or(Writer::External));
        }
        out
    }

    /// Changes since `seen.base` that the agent has not re-read at the current version.
    pub fn unacked(&self, seen: &Seen, memo: &DiffMemo) -> Vec<Change> {
        unacked_changes(
            &seen.base,
            &self.current,
            &seen.fresh,
            seen.fresh_hash == self.current.hash,
            memo,
        )
    }

    pub fn drift(&self, seen: &Seen, changes: Vec<Change>) -> Drift {
        Drift {
            writers: self.writers_since(seen.base.hash),
            changes,
            line_count: self.current.line_count(),
            coarse: seen.base.whole_only || self.current.whole_only,
        }
    }
}

/// Changes from `base` to `current` that `fresh` (lines re-read at the current
/// version when `fresh_now`) does not cover. The diff comes from `memo`, so a caller
/// that prefetched it outside the state lock pays only a lookup.
pub(super) fn unacked_changes(
    base: &Snapshot,
    current: &Snapshot,
    fresh: &LineRanges,
    fresh_now: bool,
    memo: &DiffMemo,
) -> Vec<Change> {
    if base.hash == current.hash {
        return Vec::new();
    }
    let n = current.line_count();
    memo.diff(base, current)
        .iter()
        .filter(|c| !(fresh_now && fresh.covers(&visible(&c.new, n))))
        .cloned()
        .collect()
}
