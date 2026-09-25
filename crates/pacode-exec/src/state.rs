use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pacode_types::{ExecConfig, TaskId, TaskInfo};
use tokio::sync::broadcast;

use crate::buffer::HeadTailBuffer;
use crate::manager::TaskEvent;
use crate::progress::ProgressParser;

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
}

pub(crate) struct SharedState {
    pub(crate) spool_dir: PathBuf,
    pub(crate) config: ExecConfig,
    pub(crate) tasks: Mutex<HashMap<TaskId, TaskEntry>>,
    pub(crate) pending_admissions: Mutex<usize>,
    pub(crate) events: broadcast::Sender<TaskEvent>,
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
