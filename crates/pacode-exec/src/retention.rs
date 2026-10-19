//! Bounded retention of finished tasks (F1) and spool garbage collection triggers (F5).
//!
//! A terminal task used to keep its whole `TaskEntry` (head/tail buffer, progress parser,
//! watch channel) forever. Now the newest [`MAX_TERMINAL`] terminal tasks keep their
//! entry; older ones are replaced by a plain `TaskInfo` stub in [`Retention`], without
//! buffer or parser.
//!
//! - Owner: `SharedState::retention` (one per `TaskManager`); fed by the supervisor when a
//!   task ends.
//! - Caps: [`MAX_TERMINAL`] = 256 terminal entries in `SharedState::tasks`;
//!   [`MAX_STUBS`] = 1024 stubs (the oldest stub is dropped beyond that; `info` of such a
//!   task is `None`).
//! - Invalidation: `TaskManager::forget` drops the stub too; nothing else expires, and
//!   nothing ticks while idle (all work happens on a task's `Ended`).
//! - Lock order: `tasks` before `retention`; neither is held across an await.
//! - `tail` of a stubbed task reads its spool file; once the spool was garbage collected
//!   (see `spool.rs`) that tail is empty.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pacode_types::{SessionId, TaskId, TaskInfo, TaskStatus};

use crate::spool;
use crate::state::SharedState;

/// Terminal tasks that keep their full `TaskEntry`.
pub(crate) const MAX_TERMINAL: usize = 256;
/// Evicted-task stubs kept for `info`/`list`.
pub(crate) const MAX_STUBS: usize = 1024;
/// A spool GC runs after this many finished tasks.
const GC_EVERY: u64 = 64;

#[derive(Default)]
struct Inner {
    /// Terminal tasks still holding an entry, oldest first.
    order: VecDeque<TaskId>,
    stubs: HashMap<TaskId, TaskInfo>,
    stub_order: VecDeque<TaskId>,
    ended: u64,
}

#[derive(Default)]
pub(crate) struct Retention {
    inner: Mutex<Inner>,
    gc_running: AtomicBool,
}

impl Retention {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a newly terminal task. Returns the tasks to evict and whether a spool GC is
    /// due.
    fn note_ended(&self, id: TaskId) -> (Vec<TaskId>, bool) {
        let mut inner = self.lock();
        inner.order.push_back(id);
        let mut victims = Vec::new();
        while inner.order.len() > MAX_TERMINAL {
            if let Some(v) = inner.order.pop_front() {
                victims.push(v);
            }
        }
        inner.ended += 1;
        (victims, inner.ended.is_multiple_of(GC_EVERY))
    }

    fn add_stubs(&self, infos: Vec<TaskInfo>) {
        let mut inner = self.lock();
        for info in infos {
            inner.stub_order.push_back(info.id.clone());
            inner.stubs.insert(info.id.clone(), info);
        }
        while inner.stub_order.len() > MAX_STUBS {
            if let Some(old) = inner.stub_order.pop_front() {
                inner.stubs.remove(&old);
            }
        }
    }

    /// The stub of an evicted task.
    pub(crate) fn get(&self, id: &TaskId) -> Option<TaskInfo> {
        self.lock().stubs.get(id).cloned()
    }

    /// All stubs, optionally for one session.
    pub(crate) fn stubs(&self, session: Option<&SessionId>) -> Vec<TaskInfo> {
        self.lock()
            .stubs
            .values()
            .filter(|i| session.is_none_or(|s| i.session == *s))
            .cloned()
            .collect()
    }

    /// Mark a stub acked; false when there is no stub.
    pub(crate) fn ack(&self, id: &TaskId) -> bool {
        match self.lock().stubs.get_mut(id) {
            Some(info) => {
                info.acked = true;
                true
            }
            None => false,
        }
    }

    pub(crate) fn forget(&self, id: &TaskId) {
        self.lock().stubs.remove(id);
    }
}

/// Called by the supervisor after the `Ended` event: evicts the oldest terminal tasks
/// beyond [`MAX_TERMINAL`] and triggers a spool GC every 64th finished task.
pub(crate) fn on_ended(state: &Arc<SharedState>, id: &TaskId) {
    let (victims, gc_due) = state.retention.note_ended(id.clone());
    if !victims.is_empty() {
        let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let mut infos = Vec::with_capacity(victims.len());
        for v in &victims {
            let terminal = tasks.get(v).is_some_and(|e| e.info.status.is_terminal());
            if terminal && let Some(entry) = tasks.remove(v) {
                infos.push(entry.info);
            }
        }
        // Still holding `tasks`: a reader never sees the task in neither place.
        state.retention.add_stubs(infos);
    }
    if gc_due {
        spawn_gc(state);
    }
}

/// Run a spool GC on the blocking pool (at most one at a time). The spools of running
/// tasks are kept.
fn spawn_gc(state: &Arc<SharedState>) {
    if state.retention.gc_running.swap(true, Ordering::AcqRel) {
        return;
    }
    let keep: HashSet<TaskId> = {
        let tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks
            .values()
            .filter(|e| e.info.status == TaskStatus::Running)
            .map(|e| e.info.id.clone())
            .collect()
    };
    let dir = state.spool_dir.clone();
    let st = Arc::clone(state);
    let job = move || {
        spool::gc(&dir, spool::SPOOL_MAX_TOTAL_BYTES, &keep);
        st.retention.gc_running.store(false, Ordering::Release);
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(job);
        }
        Err(_) => job(),
    }
}

/// Startup GC (`TaskManager::with_resources`); no task runs yet, so nothing is kept.
pub(crate) fn gc_at_start(state: &Arc<SharedState>) {
    spawn_gc(state);
}

/// Last `max_chars` characters of a spool file (fallback for an evicted task).
pub(crate) async fn spool_tail(path: &std::path::Path, max_chars: usize) -> String {
    let content = tokio::fs::read(path).await.unwrap_or_default();
    let text = String::from_utf8_lossy(&content);
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max_chars)).collect()
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod retention_tests;
