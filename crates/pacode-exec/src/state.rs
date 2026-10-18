use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use pacode_types::{ExecConfig, TaskId, TaskInfo, TaskStatus};
use tokio::sync::broadcast;

use crate::buffer::HeadTailBuffer;
use crate::manager::TaskEvent;
use crate::progress::ProgressParser;
use crate::resources::{GuardHost, KillSignal, ResourceGuard, ResourceNotice, TreeRoot};

pub(crate) struct TaskEntry {
    pub(crate) info: TaskInfo,
    pub(crate) buffer: HeadTailBuffer,
    pub(crate) parser: ProgressParser,
    pub(crate) version_tx: tokio::sync::watch::Sender<u64>,
    pub(crate) last_activity: std::time::Instant,
    pub(crate) stalled: bool,
    pub(crate) kill_requested: bool,
    pub(crate) child_pid: Option<u32>,
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
