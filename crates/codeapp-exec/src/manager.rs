//! `TaskManager`: owns every running process of the daemon.
//!
//! Implementation notes:
//! - spawn via `sh -c` (or `$SHELL -c`), `process_group(0)` (own group), stdin null,
//!   stdout+stderr piped; a reader task per pipe appends to the spool file and the
//!   in-RAM [`HeadTailBuffer`], feeds [`ProgressParser`] line by line, and touches the
//!   stall clock;
//! - spool file `spool_dir/<task_id>.out` (append, 0600), truncated with a marker at
//!   `max_spool_bytes`;
//! - stall watchdog: one `tokio::time::sleep` re-armed on activity, fires at most once
//!   per silence episode;
//! - `kill`: SIGTERM to the process group, `kill_grace_secs`, then SIGKILL;
//! - events on a `tokio::sync::broadcast` channel (capacity 256, lagging receivers just
//!   miss events and should re-list);
//! - `shutdown` kills everything and waits for reader tasks.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codeapp_types::{ExecConfig, SessionId, TaskId, TaskInfo, TaskProgress};
use tokio::sync::broadcast;

use crate::spec::TaskSpec;

#[derive(Clone, Debug, PartialEq)]
pub enum TaskEvent {
    Started(TaskInfo),
    Progress(TaskInfo),
    Ended(TaskInfo),
    /// No output and no progress for `exec.stall_secs`; the task is still running.
    Stalled(TaskInfo),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WaitResult {
    Ended(TaskInfo),
    Progress(TaskInfo),
    Timeout(TaskInfo),
    NotFound,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("task not found: {0}")]
    NotFound(TaskId),
    #[error("too many tasks ({0})")]
    TooManyTasks(usize),
    #[error("spawn failed: {0}")]
    Spawn(std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct TaskManager {
    _private: (),
}

impl TaskManager {
    pub fn new(spool_dir: PathBuf, config: ExecConfig) -> Arc<Self> {
        let _ = (spool_dir, config);
        todo!("TaskManager::new")
    }

    /// Start the process. Returns immediately with the initial `TaskInfo`.
    pub async fn spawn(&self, spec: TaskSpec) -> Result<TaskInfo, ExecError> {
        let _ = spec;
        todo!("TaskManager::spawn")
    }

    pub fn info(&self, id: &TaskId) -> Option<TaskInfo> {
        let _ = id;
        todo!("TaskManager::info")
    }

    /// All tasks (running and finished, until `forget`), optionally for one session,
    /// ordered by start time.
    pub fn list(&self, session: Option<&SessionId>) -> Vec<TaskInfo> {
        let _ = session;
        todo!("TaskManager::list")
    }

    /// Wait until the task ends, `timeout` elapses, or (when `return_on_progress`) the
    /// progress changes.
    pub async fn wait(
        &self,
        id: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitResult {
        let _ = (id, timeout, return_on_progress);
        todo!("TaskManager::wait")
    }

    pub async fn kill(&self, id: &TaskId) -> Result<(), ExecError> {
        let _ = id;
        todo!("TaskManager::kill")
    }

    /// Kill every running task of a session (session close).
    pub async fn kill_session(&self, session: &SessionId) {
        let _ = session;
        todo!("TaskManager::kill_session")
    }

    /// Last `lines` lines (from RAM tail; falls back to the spool file).
    pub async fn tail(&self, id: &TaskId, lines: usize) -> Result<Vec<String>, ExecError> {
        let _ = (id, lines);
        todo!("TaskManager::tail")
    }

    /// Head+tail rendering capped to `max_chars` (for tool results).
    pub async fn output(&self, id: &TaskId, max_chars: usize) -> Result<String, ExecError> {
        let _ = (id, max_chars);
        todo!("TaskManager::output")
    }

    /// Agent-reported progress (`bg progress`). Wins over parsed progress.
    pub fn report_progress(&self, id: &TaskId, progress: TaskProgress) -> Result<(), ExecError> {
        let _ = (id, progress);
        todo!("TaskManager::report_progress")
    }

    /// The foreground wait gave up: the task now shows in the BACKGROUND rail.
    pub fn mark_backgrounded(&self, id: &TaskId) {
        let _ = id;
        todo!("TaskManager::mark_backgrounded")
    }

    /// Clear the failed-unacked flag.
    pub fn ack(&self, id: &TaskId) {
        let _ = id;
        todo!("TaskManager::ack")
    }

    /// Drop a finished task from the list (its spool file stays).
    pub fn forget(&self, id: &TaskId) {
        let _ = id;
        todo!("TaskManager::forget")
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        todo!("TaskManager::subscribe")
    }

    /// Kill all tasks and stop reader tasks.
    pub async fn shutdown(&self) {
        todo!("TaskManager::shutdown")
    }
}
