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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codeapp_types::{
    ExecConfig, ProgressSource, SessionId, TaskId, TaskInfo, TaskProgress, TaskStatus,
};
use tokio::sync::broadcast;

use crate::buffer::HeadTailBuffer;
use crate::progress::ProgressParser;
use crate::spec::TaskSpec;
use crate::state::{SharedState, TaskEntry};

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
    state: Arc<SharedState>,
}

impl TaskManager {
    pub fn new(spool_dir: PathBuf, config: ExecConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            state: Arc::new(SharedState {
                spool_dir,
                config,
                tasks: Mutex::new(HashMap::new()),
                events,
            }),
        })
    }

    /// Start the process. Returns immediately with the initial `TaskInfo`.
    pub async fn spawn(&self, spec: TaskSpec) -> Result<TaskInfo, ExecError> {
        // Enforce max_tasks on currently running tasks
        {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let running = tasks
                .values()
                .filter(|e| e.info.status == TaskStatus::Running)
                .count();
            if running >= self.state.config.max_tasks {
                return Err(ExecError::TooManyTasks(self.state.config.max_tasks));
            }
        }

        tokio::fs::create_dir_all(&self.state.spool_dir)
            .await
            .map_err(ExecError::Spawn)?;

        let id = TaskId::generate();
        let spool_path = self.state.spool_dir.join(format!("{}.out", id.as_str()));

        let spool_file = {
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let std_file = opts.open(&spool_path).map_err(ExecError::Spawn)?;
            tokio::fs::File::from_std(std_file)
        };

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg("-c").arg(&spec.command);
        cmd.current_dir(&spec.cwd);
        cmd.envs(spec.env.iter().cloned());
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(false);

        let child = cmd.spawn().map_err(ExecError::Spawn)?;
        let child_pid = child.id();

        let label = spec
            .label
            .clone()
            .unwrap_or_else(|| codeapp_types::task_label_from_command(&spec.command));
        let now_ms = codeapp_types::now_ms();
        let info = TaskInfo {
            id: id.clone(),
            session: spec.session.clone(),
            owner: spec.owner.clone(),
            label,
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
            status: TaskStatus::Running,
            backgrounded: spec.background,
            exit_code: None,
            started_at_ms: now_ms,
            ended_at_ms: None,
            progress: None,
            warnings: 0,
            errors: 0,
            output_path: spool_path.clone(),
            output_bytes: 0,
            acked: false,
        };

        let (version_tx, _) = tokio::sync::watch::channel(0u64);
        let head_tail_buf = HeadTailBuffer::new(16 * 1024, self.state.config.tail_bytes);
        let parser = ProgressParser::new();

        let entry = TaskEntry {
            info: info.clone(),
            buffer: head_tail_buf,
            parser,
            version_tx,
            last_activity: std::time::Instant::now(),
            stalled: false,
            kill_requested: false,
            child_pid,
            supervisor: None,
        };

        {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.insert(id.clone(), entry);
        }

        let _ = self.state.events.send(TaskEvent::Started(info.clone()));

        let supervisor = tokio::spawn(crate::supervisor::run_supervisor(
            id.clone(),
            child,
            spool_file,
            spec.timeout,
            Arc::clone(&self.state),
        ));

        {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = tasks.get_mut(&id) {
                entry.supervisor = Some(supervisor);
            }
        }

        Ok(info)
    }

    pub fn info(&self, id: &TaskId) -> Option<TaskInfo> {
        let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.get(id).map(|e| e.info.clone())
    }

    /// All tasks (running and finished, until `forget`), optionally for one session,
    /// ordered by start time.
    pub fn list(&self, session: Option<&SessionId>) -> Vec<TaskInfo> {
        let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let mut result: Vec<TaskInfo> = tasks
            .values()
            .filter(|e| match session {
                Some(s) => e.info.session == *s,
                None => true,
            })
            .map(|e| e.info.clone())
            .collect();
        result.sort_by_key(|e| e.started_at_ms);
        result
    }

    /// Wait until the task ends, `timeout` elapses, or (when `return_on_progress`) the
    /// progress changes.
    pub async fn wait(
        &self,
        id: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitResult {
        let (mut rx, initial_info, initial_progress) = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            match tasks.get(id) {
                Some(entry) => {
                    if entry.info.status.is_terminal() {
                        return WaitResult::Ended(entry.info.clone());
                    }
                    (
                        entry.version_tx.subscribe(),
                        entry.info.clone(),
                        entry.info.progress.clone(),
                    )
                }
                None => return WaitResult::NotFound,
            }
        };

        if timeout.is_zero() {
            return WaitResult::Timeout(initial_info);
        }

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                let current = self.info(id).unwrap_or(initial_info);
                return WaitResult::Timeout(current);
            }

            match tokio::time::timeout(remaining, rx.changed()).await {
                Err(_) => {
                    let current = self.info(id).unwrap_or(initial_info);
                    return WaitResult::Timeout(current);
                }
                Ok(Err(_)) => {
                    let current = match self.info(id) {
                        Some(info) => info,
                        None => return WaitResult::NotFound,
                    };
                    if current.status.is_terminal() {
                        return WaitResult::Ended(current);
                    }
                    return WaitResult::Timeout(current);
                }
                Ok(Ok(())) => {
                    let current = match self.info(id) {
                        Some(info) => info,
                        None => return WaitResult::NotFound,
                    };
                    if current.status.is_terminal() {
                        return WaitResult::Ended(current);
                    }
                    if return_on_progress && current.progress != initial_progress {
                        return WaitResult::Progress(current);
                    }
                }
            }
        }
    }

    pub async fn kill(&self, id: &TaskId) -> Result<(), ExecError> {
        let (pid, mut rx) = {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let entry = tasks
                .get_mut(id)
                .ok_or_else(|| ExecError::NotFound(id.clone()))?;
            if entry.info.status.is_terminal() {
                return Ok(());
            }
            entry.kill_requested = true;
            (entry.child_pid, entry.version_tx.subscribe())
        };

        if let Some(pid) = pid {
            #[cfg(unix)]
            unsafe {
                libc::killpg(pid as i32, libc::SIGTERM);
            }

            let grace_secs = self.state.config.kill_grace_secs;
            if grace_secs > 0 {
                wait_terminal(
                    id,
                    &self.state.tasks,
                    &mut rx,
                    Duration::from_secs(grace_secs),
                )
                .await;
            }

            let still_running = {
                let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
                tasks
                    .get(id)
                    .map(|e| e.info.status == TaskStatus::Running)
                    .unwrap_or(false)
            };

            if still_running {
                #[cfg(unix)]
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
                wait_terminal(id, &self.state.tasks, &mut rx, Duration::from_secs(2)).await;
            }
        }

        Ok(())
    }

    /// Kill every running task of a session (session close).
    pub async fn kill_session(&self, session: &SessionId) {
        let to_kill: Vec<TaskId> = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks
                .values()
                .filter(|e| e.info.session == *session && e.info.status == TaskStatus::Running)
                .map(|e| e.info.id.clone())
                .collect()
        };
        for id in to_kill {
            let _ = self.kill(&id).await;
        }
    }

    /// Last `lines` lines (from RAM tail; falls back to the spool file).
    pub async fn tail(&self, id: &TaskId, lines: usize) -> Result<Vec<String>, ExecError> {
        let (buffer_lines, spool_path) = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let entry = tasks
                .get(id)
                .ok_or_else(|| ExecError::NotFound(id.clone()))?;
            (
                entry.buffer.tail_lines(lines),
                entry.info.output_path.clone(),
            )
        };

        if !buffer_lines.is_empty() {
            return Ok(buffer_lines);
        }

        match tokio::fs::read_to_string(&spool_path).await {
            Ok(content) => {
                let all_lines: Vec<&str> = content.lines().collect();
                let start = all_lines.len().saturating_sub(lines);
                Ok(all_lines[start..].iter().map(|s| s.to_string()).collect())
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Head+tail rendering capped to `max_chars` (for tool results).
    pub async fn output(&self, id: &TaskId, max_chars: usize) -> Result<String, ExecError> {
        let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let entry = tasks
            .get(id)
            .ok_or_else(|| ExecError::NotFound(id.clone()))?;
        Ok(entry.buffer.render(max_chars))
    }

    /// Agent-reported progress (`bg progress`). Wins over parsed progress.
    pub fn report_progress(&self, id: &TaskId, progress: TaskProgress) -> Result<(), ExecError> {
        let info = {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let entry = tasks
                .get_mut(id)
                .ok_or_else(|| ExecError::NotFound(id.clone()))?;
            let mut reported = progress;
            reported.source = ProgressSource::Reported;
            entry.info.progress = Some(reported);
            entry.version_tx.send_modify(|v| *v += 1);
            entry.info.clone()
        };

        let _ = self.state.events.send(TaskEvent::Progress(info));
        Ok(())
    }

    /// The foreground wait gave up: the task now shows in the BACKGROUND rail.
    pub fn mark_backgrounded(&self, id: &TaskId) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.info.backgrounded = true;
        }
    }

    /// Clear the failed-unacked flag.
    pub fn ack(&self, id: &TaskId) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.info.acked = true;
        }
    }

    /// Drop a finished task from the list (its spool file stays).
    pub fn forget(&self, id: &TaskId) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.remove(id);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.state.events.subscribe()
    }

    /// Kill all tasks and stop reader tasks.
    pub async fn shutdown(&self) {
        let to_kill: Vec<TaskId> = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks
                .values()
                .filter(|e| e.info.status == TaskStatus::Running)
                .map(|e| e.info.id.clone())
                .collect()
        };
        for id in to_kill {
            let _ = self.kill(&id).await;
        }

        let handles: Vec<tokio::task::JoinHandle<()>> = {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks
                .values_mut()
                .filter_map(|e| e.supervisor.take())
                .collect()
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

async fn wait_terminal(
    id: &TaskId,
    tasks: &Mutex<HashMap<TaskId, TaskEntry>>,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    timeout: Duration,
) {
    let _ = tokio::time::timeout(timeout, async {
        loop {
            {
                let t = tasks.lock().unwrap_or_else(|e| e.into_inner());
                if t.get(id).is_none_or(|e| e.info.status.is_terminal()) {
                    break;
                }
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod manager_tests;
