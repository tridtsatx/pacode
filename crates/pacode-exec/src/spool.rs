//! Spool directory garbage collection (F5).
//!
//! Each task writes `<spool_dir>/<task_id>.out` (at most `exec.max_spool_bytes`). The
//! manager deletes the spool of a task it evicts or forgets, but spools of tasks it no
//! longer knows (a crash, a daemon restart, a session never reopened, so never
//! `hydrate`d) used to stay forever. [`gc`] sweeps those:
//!
//! - a spool of an unknown task untouched for [`SPOOL_STALE_AFTER`] is deleted;
//! - beyond [`SPOOL_MAX_TOTAL_BYTES`] over all `*.out` files, the oldest unknown ones go
//!   first until the total fits.
//!
//! Owner: `SharedState::spool_gc` (one per `TaskManager`).
//! - Protected: the spool of every task in `SharedState::tasks` (running or retained
//!   terminal), and any spool written within [`SPOOL_MIN_AGE`] (a spawn creates its file
//!   just before its entry is inserted; another daemon may share the directory).
//! - Triggers: manager construction, then on a task's end once [`GC_EVERY`] tasks ended
//!   or [`GC_MAX_INTERVAL`] passed since the last run. No timer: nothing ticks while
//!   idle. At most one run at a time, on the blocking pool.
//! - A deleted spool means a task `hydrate`d later has an empty `tail`; its
//!   `TaskInfo::output_path` then points at a missing file.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pacode_types::TaskId;

use crate::state::SharedState;

/// Total size of `*.out` spool files kept on disk.
pub(crate) const SPOOL_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// A spool of an unknown task untouched this long is stale.
pub(crate) const SPOOL_STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
/// A spool written this recently is never deleted.
pub(crate) const SPOOL_MIN_AGE: Duration = Duration::from_secs(5 * 60);
/// A GC runs after this many finished tasks...
const GC_EVERY: u64 = 64;
/// ...or on the first end this long after the previous run.
const GC_MAX_INTERVAL: Duration = Duration::from_secs(3600);

/// What [`gc`] deletes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GcLimits {
    pub(crate) max_total_bytes: u64,
    pub(crate) stale_after: Duration,
    pub(crate) min_age: Duration,
}

impl GcLimits {
    pub(crate) const DEFAULT: Self = Self {
        max_total_bytes: SPOOL_MAX_TOTAL_BYTES,
        stale_after: SPOOL_STALE_AFTER,
        min_age: SPOOL_MIN_AGE,
    };
}

/// Delete stale and over-cap `*.out` files in `dir` (see the module doc). A file whose
/// stem `is_known` (a task the manager holds; asked right before each delete, so a task
/// hydrated during the sweep keeps its spool) is never deleted. Returns the number of
/// files removed. Blocking: call from `spawn_blocking`.
pub(crate) fn gc(
    dir: &Path,
    limits: GcLimits,
    is_known: impl Fn(&TaskId) -> bool,
    now: SystemTime,
) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(SystemTime, u64, std::path::PathBuf, TaskId)> = Vec::new();
    let mut total: u64 = 0;
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("out") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let id = TaskId::new(stem);
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total = total.saturating_add(meta.len());
        files.push((modified, meta.len(), path, id));
    }
    files.sort_by_key(|f| f.0);
    let mut removed = 0;
    for (modified, len, path, id) in files {
        // A modification time in the future reads as age zero: kept.
        let age = now.duration_since(modified).unwrap_or_default();
        if age < limits.min_age {
            // Oldest first: every later file is younger still.
            break;
        }
        if age < limits.stale_after && total <= limits.max_total_bytes {
            // Same: no later file is stale, and the total fits.
            break;
        }
        if is_known(&id) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
            removed += 1;
        }
    }
    removed
}

#[derive(Debug)]
struct GcClock {
    ends_since_run: u64,
    last_run: Instant,
}

/// When the next spool GC is due, and whether one is running.
#[derive(Debug)]
pub(crate) struct SpoolGc {
    clock: Mutex<GcClock>,
    running: AtomicBool,
}

impl Default for SpoolGc {
    fn default() -> Self {
        Self {
            clock: Mutex::new(GcClock {
                ends_since_run: 0,
                last_run: Instant::now(),
            }),
            running: AtomicBool::new(false),
        }
    }
}

impl SpoolGc {
    /// Count one finished task; true when a run is due (and the clock restarts).
    fn note_ended(&self) -> bool {
        let mut clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.ends_since_run += 1;
        if clock.ends_since_run < GC_EVERY && clock.last_run.elapsed() < GC_MAX_INTERVAL {
            return false;
        }
        clock.ends_since_run = 0;
        clock.last_run = Instant::now();
        true
    }
}

/// Called by the supervisor after a task's `Ended` event.
pub(crate) fn on_ended(state: &Arc<SharedState>) {
    if state.spool_gc.note_ended() {
        spawn_gc(state);
    }
}

/// Startup sweep (`TaskManager::with_limits`): spools of earlier runs.
pub(crate) fn gc_at_start(state: &Arc<SharedState>) {
    spawn_gc(state);
}

/// Run a GC on the blocking pool (at most one at a time). The spools of every task the
/// manager still holds, running or retained, are kept.
fn spawn_gc(state: &Arc<SharedState>) {
    if state.spool_gc.running.swap(true, Ordering::AcqRel) {
        return;
    }
    let dir = state.spool_dir.clone();
    let st = Arc::clone(state);
    let job = move || {
        let is_known = |id: &TaskId| {
            st.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(id)
        };
        gc(&dir, GcLimits::DEFAULT, is_known, SystemTime::now());
        st.spool_gc.running.store(false, Ordering::Release);
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(job);
        }
        Err(_) => job(),
    }
}

#[cfg(test)]
#[path = "spool_tests.rs"]
mod spool_tests;
