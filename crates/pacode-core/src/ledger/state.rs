//! The pure ledger: per path, the current version, who wrote it, and what each agent
//! has seen. No I/O and no locking; [`super::FileLedger`] wraps it in a mutex and
//! feeds it file contents read under the path's write lock.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_types::AgentId;

use super::ranges::{LineRanges, overlaps, visible};
use super::snapshot::{Change, Snapshot, Stat, diff};

/// Tracked paths per session; past it the least recently used path is forgotten
/// (with every agent's seen state for it, so "read first" applies to it again).
pub const MAX_PATHS: usize = 2048;

/// Versions remembered per path to attribute changes to their writers.
const MAX_LOG: usize = 32;

/// Who produced a version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writer {
    Agent {
        id: AgentId,
        name: String,
    },
    /// Not written through pacode's tools (user editor, another program).
    External,
}

impl Writer {
    pub fn agent(id: &AgentId, name: &str) -> Self {
        Self::Agent {
            id: id.clone(),
            name: name.to_string(),
        }
    }

    /// `agent reviewer`, `agent main (you)` or `external (...)`.
    pub fn describe(&self, viewer: &AgentId) -> String {
        match self {
            Self::Agent { id, name } if id == viewer => format!("agent {name} (you)"),
            Self::Agent { name, .. } => format!("agent {name}"),
            Self::External => "external (an editor or a process outside pacode)".to_string(),
        }
    }
}

/// What changed since an agent's read that it has not re-read: the writers since its
/// version and the changed regions (`new` side = current line numbers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub writers: Vec<Writer>,
    pub changes: Vec<Change>,
    /// Lines in the current version.
    pub line_count: usize,
}

/// Outcome of [`LedgerState::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The file is new, or nothing changed since the agent's read.
    Clean,
    /// The file changed since the agent's read, but not on lines this write touches.
    Disjoint(Drift),
    /// The file exists and the agent has no seen state for it.
    NotRead,
    /// Changes the agent has not seen overlap the write (or the edit failed).
    Conflict(Drift),
}

/// One row of an agent's digest (compaction pin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestRow {
    pub path: PathBuf,
    pub version: String,
    pub wrote: bool,
    pub read: LineRanges,
    pub drift: Option<Drift>,
}

struct LogEntry {
    to_hash: u64,
    writer: Writer,
}

/// One agent's view of one path.
#[derive(Clone)]
struct Seen {
    /// The version the agent is in sync with: changes after it are drift.
    base: Arc<Snapshot>,
    /// Lines read (or shown in a rejection) at version `fresh_hash`.
    fresh: LineRanges,
    fresh_hash: u64,
    /// Its last action on the path was a write.
    wrote: bool,
    /// Ledger tick of its last action on the path (digest order).
    at: u64,
}

impl Seen {
    fn at_version(snap: Arc<Snapshot>, tick: u64) -> Self {
        Self {
            fresh_hash: snap.hash,
            base: snap,
            fresh: LineRanges::default(),
            wrote: false,
            at: tick,
        }
    }
}

struct PathEntry {
    current: Arc<Snapshot>,
    stat: Stat,
    last_writer: Option<Writer>,
    log: VecDeque<LogEntry>,
    seen: HashMap<AgentId, Seen>,
    last_used: u64,
}

impl PathEntry {
    fn new(current: Arc<Snapshot>, stat: Stat, tick: u64) -> Self {
        Self {
            current,
            stat,
            last_writer: None,
            log: VecDeque::new(),
            seen: HashMap::new(),
            last_used: tick,
        }
    }

    /// Move to `next` written by `writer`; only the stat changes when the content is
    /// the same.
    fn advance(&mut self, next: Arc<Snapshot>, stat: Stat, writer: Writer) {
        self.stat = stat;
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
    fn writers_since(&self, base_hash: u64) -> Vec<Writer> {
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
    fn unacked(&self, seen: &Seen) -> Vec<Change> {
        if seen.base.hash == self.current.hash {
            return Vec::new();
        }
        let n = self.current.lines.len();
        let fresh_now = seen.fresh_hash == self.current.hash;
        diff(&seen.base.lines, &self.current.lines)
            .into_iter()
            .filter(|c| !(fresh_now && seen.fresh.covers(&visible(&c.new, n))))
            .collect()
    }

    fn drift(&self, seen: &Seen, changes: Vec<Change>) -> Drift {
        Drift {
            writers: self.writers_since(seen.base.hash),
            changes,
            line_count: self.current.lines.len(),
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
        }
    }

    pub fn tracked_count(&self) -> usize {
        self.entries.len()
    }

    pub fn is_tracked(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
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
        let (entry, tick) = self.sync(path, snap, stat, Writer::External);
        let current = entry.current.clone();
        let whole = lines.start == 0 && lines.end >= current.lines.len();
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
        if whole || entry.unacked(&seen).is_empty() {
            seen.base = current;
        }
        entry.seen.insert(agent.clone(), seen);
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
        // A content mismatch with the last known version is an external change.
        let (entry, _) = self.sync(path, snap, stat, Writer::External);
        let Some(seen) = entry.seen.get(agent).cloned() else {
            return Verdict::NotRead;
        };
        let changes = entry.unacked(&seen);
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
        if entry.unacked(&seen).is_empty() {
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
        let writer = Writer::agent(agent, name);
        let created = !self.entries.contains_key(path);
        let before = self.entries.get(path).map(|e| e.current.clone());
        let (entry, tick) = self.sync(path, snap.clone(), stat, writer.clone());
        let mut fresh = LineRanges::default();
        match before {
            Some(prev) => {
                for c in diff(&prev.lines, &snap.lines) {
                    fresh.insert(c.new);
                }
            }
            None => fresh.insert(0..snap.lines.len()),
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

    /// The paths `agent` has seen, most recent first, at most `limit`, plus the
    /// number left out.
    pub fn digest(&self, agent: &AgentId, limit: usize) -> (Vec<DigestRow>, usize) {
        let mut rows: Vec<(u64, DigestRow)> = Vec::new();
        for (path, entry) in &self.entries {
            let Some(seen) = entry.seen.get(agent) else {
                continue;
            };
            let changes = entry.unacked(seen);
            let drift = (!changes.is_empty()).then(|| entry.drift(seen, changes));
            let read = if seen.fresh_hash == entry.current.hash {
                seen.fresh.clone()
            } else {
                LineRanges::default()
            };
            rows.push((
                seen.at,
                DigestRow {
                    path: path.clone(),
                    version: entry.current.short(),
                    wrote: seen.wrote,
                    read,
                    drift,
                },
            ));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let omitted = rows.len().saturating_sub(limit);
        let rows = rows.into_iter().take(limit).map(|(_, r)| r).collect();
        (rows, omitted)
    }
}
