use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pacode_types::{ExecConfig, TaskId, TaskInfo, TaskStatus};
use tokio::sync::broadcast;

use crate::admission::Admission;
use crate::buffer::HeadTailBuffer;
use crate::manager::TaskEvent;
use crate::progress::ProgressParser;
use crate::resources::{GuardHost, KillSignal, ResourceGuard, ResourceNotice, TreeRoot};
use crate::retention::Retention;

pub(crate) struct ManagedSpool {
    path: PathBuf,
    remove_on_cleanup: bool,
    cleanup_requested: AtomicBool,
}

impl ManagedSpool {
    pub(crate) fn new(path: PathBuf, remove_on_cleanup: bool) -> Self {
        Self {
            path,
            remove_on_cleanup,
            cleanup_requested: AtomicBool::new(false),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn request_cleanup(&self) {
        self.cleanup_requested.store(true, Ordering::Relaxed);
    }
}

impl Drop for ManagedSpool {
    fn drop(&mut self) {
        if self.remove_on_cleanup && self.cleanup_requested.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) struct TaskEntry {
    pub(crate) info: TaskInfo,
    /// [`crate::spec::TaskSpec::call`], carried on every `Output` event.
    pub(crate) call: Option<pacode_types::CallId>,
    pub(crate) spool: Arc<ManagedSpool>,
    pub(crate) buffer: HeadTailBuffer,
    pub(crate) parser: ProgressParser,
    pub(crate) version_tx: tokio::sync::watch::Sender<u64>,
    pub(crate) last_activity: std::time::Instant,
    pub(crate) stalled: bool,
    pub(crate) kill_requested: bool,
    pub(crate) child_pid: Option<u32>,
    pub(crate) kill_tx: tokio::sync::watch::Sender<bool>,
    pub(crate) supervisor: Option<tokio::task::JoinHandle<()>>,
    /// Spawn time, for the resource guard's young-tree protection.
    pub(crate) started: std::time::Instant,
    /// Never chosen as a resource-guard victim.
    pub(crate) protected: bool,
    /// Set when the resource guard stopped this task; reported in its output.
    pub(crate) guard_reason: Option<String>,
}

pub(crate) struct SharedState {
    pub(crate) spool_dir: PathBuf,
    pub(crate) config: ExecConfig,
    pub(crate) tasks: Mutex<HashMap<TaskId, TaskEntry>>,
    pub(crate) events: broadcast::Sender<TaskEvent>,
    pub(crate) guard: ResourceGuard,
    /// Niceness applied to spawned processes (0 = off).
    pub(crate) nice: i32,
    /// Admission queue for `exec.max_tasks` process slots (F4).
    pub(crate) admission: Arc<Admission>,
    /// Stubs of evicted terminal tasks (F1).
    pub(crate) retention: Retention,
}

impl GuardHost for SharedState {
    fn roots(&self) -> Vec<TreeRoot> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks
            .values()
            .filter(|e| e.info.status == TaskStatus::Running)
            .filter_map(|e| {
                e.child_pid.map(|pid| TreeRoot {
                    id: e.info.id.clone(),
                    pgid: pid,
                    started: e.started,
                    protected: e.protected,
                })
            })
            .collect()
    }

    fn apply_kill(&self, id: &TaskId, signal: KillSignal, reason: &str) {
        let pid = {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = tasks.get_mut(id) else {
                return;
            };
            if entry.info.status.is_terminal() {
                return;
            }
            entry.kill_requested = true;
            if entry.guard_reason.is_none() {
                entry.guard_reason = Some(reason.to_string());
            }
            entry.child_pid
        };
        #[cfg(unix)]
        {
            if let Some(pid) = pid {
                let sig = match signal {
                    KillSignal::Term => libc::SIGTERM,
                    KillSignal::Kill => libc::SIGKILL,
                };
                // SAFETY: plain signal syscall on the process group we created.
                unsafe {
                    libc::killpg(pid as i32, sig);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = (pid, signal);
    }

    fn notice(&self, notice: ResourceNotice) {
        let _ = self.events.send(TaskEvent::Resource(notice));
    }
}

impl SharedState {
    pub(crate) fn prune_terminal_overflow(
        &self,
        tasks: &mut HashMap<TaskId, TaskEntry>,
    ) -> Vec<TaskEntry> {
        let mut terminal: Vec<(u64, TaskId)> = tasks
            .iter()
            .filter(|(_, entry)| entry.info.status.is_terminal())
            .map(|(id, entry)| (entry.info.ended_at_ms.unwrap_or_default(), id.clone()))
            .collect();
        let overflow = terminal.len().saturating_sub(self.config.max_tasks);
        if overflow == 0 {
            return Vec::new();
        }

        terminal.sort_unstable_by(|(left_time, left_id), (right_time, right_id)| {
            left_time
                .cmp(right_time)
                .then_with(|| left_id.cmp(right_id))
        });
        let evicted = terminal
            .drain(..overflow)
            .filter_map(|(_, id)| tasks.remove(&id))
            .collect::<Vec<_>>();
        for entry in &evicted {
            entry.spool.request_cleanup();
        }
        evicted
    }
}
