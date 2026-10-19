//! File ownership ledger: several agents (main + subagents) write one working tree
//! without worktrees and without silently clobbering each other.
//!
//! Per canonical path the session tracks the current version (content hash is
//! authoritative; len + mtime are only a pre-check for command side effects), who
//! wrote it, and per agent what it has seen: the version it is in sync with plus the
//! lines it re-read since. Rules (enforced by the `read`/`write`/`edit`/`multi_edit`
//! tools through `ToolHost`):
//! - `read` records the agent's view; `grep`/`glob` output does not count.
//! - A write to an existing file the agent never read is rejected (`read X first`).
//! - A write over a file that changed since the agent's read passes when it touches
//!   none of the changed lines (the agent is told what changed), and is rejected
//!   with who changed which lines plus the fresh anchored lines otherwise; those
//!   lines count as re-read, so one retry is enough.
//! - A successful write moves the writer's view to the new version; other agents
//!   keep theirs and see the change as drift.
//! - Content that differs from the last known version without a pacode write is
//!   attributed to `external`; after a `bash` command, tracked paths whose stat
//!   changed are rehashed and attributed to the agent that ran it.
//! - Writers of one path are serialized by a per-path async lock held across read,
//!   check and write.
//!
//! State lives in memory per session (lost on daemon restart, so "read first"
//! applies again); nothing is persisted and nothing runs while idle. Caps: see
//! [`state::MAX_PATHS`], [`ranges::MAX_INTERVALS`] and [`locks::LOCK_MAP_CAP`].

mod locks;
mod ranges;
mod refresh;
mod render;
mod snapshot;
mod state;

use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use pacode_tools::WriteIntent;
use pacode_types::AgentId;

pub use locks::{LOCK_MAP_CAP, PathGuard};
pub use ranges::{LineRanges, MAX_INTERVALS};
pub use refresh::REFRESH_MAX_PATHS;
pub use snapshot::{Snapshot, Stat};
pub use state::{Drift, LedgerState, MAX_PATHS, Verdict, Writer};

/// The session's file ledger. Owner: `Session::ledger`.
#[derive(Default)]
pub struct FileLedger {
    state: Mutex<LedgerState>,
    locks: locks::PathLocks,
    refresh: refresh::RefreshGate,
}

impl FileLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// A ledger tracking at most `cap` paths (tests).
    pub fn with_cap(cap: usize) -> Self {
        Self {
            state: Mutex::new(LedgerState::with_cap(cap)),
            locks: locks::PathLocks::default(),
            refresh: refresh::RefreshGate::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Paths tracked right now.
    pub fn tracked_count(&self) -> usize {
        self.state().tracked_count()
    }

    pub fn is_tracked(&self, key: &Path) -> bool {
        self.state().is_tracked(key)
    }

    /// Paths with a write lock held or awaited.
    pub fn locked_count(&self) -> usize {
        self.locks.count()
    }

    /// Lock every path (canonical, deduplicated, sorted so concurrent multi-path
    /// writers cannot deadlock) until the guards drop.
    pub async fn lock(&self, paths: &[PathBuf]) -> Vec<PathGuard> {
        let mut keys = Vec::with_capacity(paths.len());
        for p in paths {
            keys.push(canonical_key(p).await);
        }
        keys.sort();
        keys.dedup();
        let mut guards = Vec::with_capacity(keys.len());
        for key in keys {
            guards.push(self.locks.lock(key).await);
        }
        guards
    }

    /// `agent` read `content` of `path` and was shown the 0-based `lines`.
    pub async fn record_read(
        &self,
        agent: &AgentId,
        path: &Path,
        content: &str,
        lines: Range<usize>,
    ) {
        let key = canonical_key(path).await;
        let stat = stat_of(&key, content.len()).await;
        let snap = Snapshot::of(content);
        self.state().record_read(agent, &key, snap, stat, lines);
    }

    /// Decide a write (see `ToolHost::ledger_check`). `shown` is the path as the
    /// model knows it. `Ok` carries an optional note for the tool output, `Err` the
    /// model-facing rejection.
    pub async fn check(
        &self,
        agent: &AgentId,
        path: &Path,
        shown: &str,
        intent: WriteIntent,
        current: Option<&str>,
        proposed: Option<&str>,
    ) -> Result<Option<String>, String> {
        let key = canonical_key(path).await;
        let now = match current {
            Some(text) => Some((Snapshot::of(text), stat_of(&key, text.len()).await)),
            None => None,
        };
        let touched = match (current, proposed) {
            (Some(c), Some(p)) => Some(snapshot::touched(c, p)),
            _ => None,
        };
        let verdict = self.state().check(agent, &key, now, touched.as_deref());
        match verdict {
            Verdict::Clean => Ok(None),
            Verdict::Disjoint(drift) => Ok(Some(render::disjoint_note(shown, &drift, agent))),
            Verdict::NotRead => Err(render::not_read(shown)),
            Verdict::Conflict(drift) => {
                let text = current.unwrap_or_default();
                let (message, shown_lines) = render::conflict(shown, &drift, agent, text, intent);
                if !shown_lines.is_empty() {
                    self.state().ack(agent, &key, &shown_lines);
                }
                Err(message)
            }
        }
    }

    /// `agent` (display `name`) wrote `content` to `path`.
    pub async fn record_write(&self, agent: &AgentId, name: &str, path: &Path, content: &str) {
        let key = canonical_key(path).await;
        let stat = stat_of(&key, content.len()).await;
        let snap = Snapshot::of(content);
        self.state().record_write(agent, name, &key, snap, stat);
    }

    /// After a command run by `agent` finished: stat the paths it may have changed
    /// (those `agent` has seen plus pacode-written ones, at most
    /// [`REFRESH_MAX_PATHS`], most recently used first) in ONE `spawn_blocking`, and
    /// rehash only those whose len/mtime moved, attributing them to `agent`. No tree
    /// walk; a path another writer holds right now is skipped (its writer records
    /// it, or the next check sees it as external). A call already covered by a
    /// sweep of the same agent that began after the call returns at once.
    pub async fn refresh_after_exec(&self, agent: &AgentId, name: &str) {
        let want = self.refresh.request();
        let Some(_sweep) = self.refresh.enter(agent, want).await else {
            return;
        };
        let candidates = self.state().refresh_candidates(agent, REFRESH_MAX_PATHS);
        if candidates.is_empty() {
            return;
        }
        let changed = tokio::task::spawn_blocking(move || refresh::changed_paths(candidates))
            .await
            .unwrap_or_default();
        for key in changed {
            let Some(_guard) = self.locks.try_lock(key.clone()) else {
                continue;
            };
            let Ok(bytes) = tokio::fs::read(&key).await else {
                continue;
            };
            self.refresh.count_rehash();
            let snap = Snapshot::of(&String::from_utf8_lossy(&bytes));
            let stat = stat_of(&key, bytes.len()).await;
            self.state().refresh(agent, name, &key, snap, stat);
        }
    }

    /// Give `to` the seen state of `from` (the main turn detached into a subagent
    /// keeps what it read).
    pub fn copy_agent(&self, from: &AgentId, to: &AgentId) {
        self.state().copy_agent(from, to);
    }

    /// Compaction pin for `agent`: the files it read or wrote recently, their
    /// versions and outstanding "modified by another agent" warnings (at most
    /// [`render::DIGEST_MAX_LINES`] lines). `None` when it has seen no files.
    pub fn digest(&self, agent: &AgentId, cwd: &Path) -> Option<String> {
        let (rows, omitted) = self.state().digest(agent, render::DIGEST_MAX_ROWS);
        let base = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let rows: Vec<(String, state::DigestRow)> = rows
            .into_iter()
            .map(|row| (shown_path(&row.path, &base), row))
            .collect();
        render::digest(&rows, omitted, agent)
    }
}

/// `path` relative to `cwd` when inside it, as the model sees paths.
pub fn shown_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// The ledger key: the canonical path, or for a path that does not exist yet the
/// canonical deepest existing ancestor joined with the rest (so a new file keeps its
/// key once created).
pub async fn canonical_key(path: &Path) -> PathBuf {
    let mut tail: Vec<&OsStr> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(mut base) = tokio::fs::canonicalize(cur).await {
            for segment in tail.iter().rev() {
                base.push(segment);
            }
            return base;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

async fn stat_of(key: &Path, fallback_len: usize) -> Stat {
    match tokio::fs::metadata(key).await {
        Ok(meta) => Stat {
            len: meta.len(),
            mtime: meta.modified().ok(),
        },
        Err(_) => Stat {
            len: u64::try_from(fallback_len).unwrap_or(u64::MAX),
            mtime: None,
        },
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod ledger_tests;
#[cfg(test)]
#[path = "ledger/refresh_tests.rs"]
mod refresh_tests;
#[cfg(test)]
#[path = "ledger/tools_tests.rs"]
mod tools_tests;
