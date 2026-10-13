//! `TaskManager`: owns every running process of the daemon.
//!
//! Implementation notes:
//! - spawn via `sh -c` (or `$SHELL -c`), `process_group(0)` (own group), stdin null,
//!   stdout+stderr piped; the supervisor drains both pipes into the spool file
//!   `spool_dir/<task_id>.out` (append, 0600, marker at `max_spool_bytes`) and the
//!   in-RAM [`HeadTailBuffer`], feeds [`ProgressParser`] line by line, and touches the
//!   stall clock;
//! - `kill`: SIGTERM to the process group, `kill_grace_secs`, then SIGKILL; a natural
//!   exit, a timeout and a cancel all leave no process of the group behind;
//! - events on a `tokio::sync::broadcast` channel (capacity 256, lagging receivers just
//!   miss events and should re-list);
//! - `shutdown` kills, escalates to SIGKILL, waits for the supervisors, and marks
//!   whatever is left as killed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacode_types::{
    AgentId, ExecConfig, ProgressSource, SessionId, TaskId, TaskInfo, TaskProgress, TaskStatus,
};
use tokio::sync::broadcast;

use crate::buffer::HeadTailBuffer;
use crate::memcap::MemoryPolicy;
use crate::memlimit::{MemoryLimiter, SpawnSetup};
use crate::progress::ProgressParser;
use crate::spec::TaskSpec;
use crate::state::{ManagedSpool, SharedState, TaskEntry};

#[derive(Clone, Debug, PartialEq)]
pub enum TaskEvent {
    Started(TaskInfo),
    Progress(TaskInfo),
    Ended(TaskInfo),
    /// No output and no progress for `exec.stall_secs`; the task is still running.
    Stalled(TaskInfo),
    /// Output the process produced, on whole lines, while it is still running.
    ///
    /// One event per batch of completed lines, not per line and not per read:
    /// a build that flushes 8 KiB at a time would otherwise put a hundred
    /// events on the wire to say one thing. A partial trailing line waits for
    /// its newline — a client that drew half a line would repaint it a moment
    /// later anyway.
    Output {
        info: TaskInfo,
        /// The foreground call the output belongs to ([`TaskSpec::call`]).
        call: Option<pacode_types::CallId>,
        text: String,
    },
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
    #[error("tool memory limit: {0}")]
    MemoryLimit(#[source] std::io::Error),
}

pub struct TaskManager {
    state: Arc<SharedState>,
    /// The machine-level memory policy every spawn of this daemon goes
    /// through. One per daemon, shared with the tools so that a raise is
    /// visible to every later spawn.
    memory: MemoryLimiter,
}

struct AdmissionGuard<'a> {
    state: &'a SharedState,
    committed: bool,
}

impl AdmissionGuard<'_> {
    fn commit(
        mut self,
        pending: &mut usize,
        entry: TaskEntry,
        tasks: &mut HashMap<TaskId, TaskEntry>,
    ) {
        if *pending > 0 {
            *pending -= 1;
        }
        tasks.insert(entry.info.id.clone(), entry);
        self.committed = true;
    }
}

impl Drop for AdmissionGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut pending = self
            .state
            .pending_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *pending > 0 {
            *pending -= 1;
        }
    }
}

impl TaskManager {
    /// Build a manager with no tool memory cap.
    ///
    /// The daemon does not use this; it supplies the user's
    /// `ui.tool_memory_percent` and `ui.tool_memory_split_sessions` through
    /// [`TaskManager::with_memory`]. A caller that reaches for `new` without a
    /// policy gets an uncapped manager rather than a guessed one — a cap
    /// derived from a default nobody chose is a cap nobody can debug.
    pub fn new(spool_dir: PathBuf, config: ExecConfig) -> Arc<Self> {
        Self::with_memory(
            spool_dir,
            config,
            MemoryLimiter::new(MemoryPolicy::disabled()),
        )
    }

    /// Build a manager that caps what a spawned process may use.
    pub fn with_memory(spool_dir: PathBuf, config: ExecConfig, memory: MemoryLimiter) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            state: Arc::new(SharedState {
                spool_dir,
                config,
                tasks: Mutex::new(HashMap::new()),
                pending_admissions: Mutex::new(0),
                events,
            }),
            memory,
        })
    }

    /// The memory policy: ceilings, the live-session division, and raises.
    pub fn memory(&self) -> &MemoryLimiter {
        &self.memory
    }

    fn try_admit(&self) -> Result<AdmissionGuard<'_>, ExecError> {
        let mut pending = self
            .state
            .pending_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tasks = self
            .state
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let running = tasks
            .values()
            .filter(|entry| entry.info.status == TaskStatus::Running)
            .count();
        if running.saturating_add(*pending) >= self.state.config.max_tasks {
            return Err(ExecError::TooManyTasks(self.state.config.max_tasks));
        }
        *pending += 1;
        drop(tasks);
        drop(pending);
        Ok(AdmissionGuard {
            state: &self.state,
            committed: false,
        })
    }

    /// Start the process. Returns immediately with the initial `TaskInfo`.
    pub async fn spawn(&self, mut spec: TaskSpec) -> Result<TaskInfo, ExecError> {
        tokio::fs::create_dir_all(&self.state.spool_dir)
            .await
            .map_err(ExecError::Spawn)?;

        let admission = self.try_admit()?;
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

        // The tool memory ceiling is decided before the process exists, so the
        // child is already inside its limit by the time its first instruction
        // runs. The environment travels with the command, so a script and the
        // model both see the ceiling they are working under.
        let plan = self.memory.plan_spawn(&spec.session);
        let mut env = std::mem::take(&mut spec.env);
        if let Some(ceiling) = &plan.ceiling {
            env.extend(ceiling.environment());
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg("-c").arg(&spec.command);
        cmd.current_dir(&spec.cwd);
        cmd.envs(env.iter().cloned());
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(false);
        if plan.setup != SpawnSetup::None {
            let setup = plan.setup.clone();
            // SAFETY: `SpawnSetup::install` is documented and implemented as
            // bare syscalls on memory allocated before the fork — no
            // allocator, no locks, no Rust runtime state.
            unsafe { cmd.pre_exec(move || setup.install()) };
        }

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                drop(spool_file);
                let _ = std::fs::remove_file(&spool_path);
                return Err(ExecError::Spawn(error));
            }
        };
        let child_pid = child.id();

        let label = spec
            .label
            .clone()
            .unwrap_or_else(|| pacode_types::task_label_from_command(&spec.command));
        let now_ms = pacode_types::now_ms();
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
        let (kill_tx, kill_rx) = tokio::sync::watch::channel(false);
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let head_tail_buf = HeadTailBuffer::new(16 * 1024, self.state.config.tail_bytes);
        let parser = ProgressParser::new();

        let mut entry = TaskEntry {
            info: info.clone(),
            call: spec.call.clone(),
            spool: Arc::new(ManagedSpool::new(spool_path, true)),
            buffer: head_tail_buf,
            parser,
            version_tx,
            last_activity: std::time::Instant::now(),
            stalled: false,
            kill_requested: false,
            child_pid,
            kill_tx,
            supervisor: None,
        };

        let supervisor_state = Arc::clone(&self.state);
        let timeout = spec.timeout;
        let supervisor = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            crate::supervisor::run_supervisor(
                id,
                child,
                spool_file,
                timeout,
                kill_rx,
                supervisor_state,
            )
            .await;
        });
        entry.supervisor = Some(supervisor);

        {
            let mut pending = self
                .state
                .pending_admissions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            admission.commit(&mut pending, entry, &mut tasks);
        }

        let _ = self.state.events.send(TaskEvent::Started(info.clone()));
        let _ = start_tx.send(());

        Ok(info)
    }

    pub fn info(&self, id: &TaskId) -> Option<TaskInfo> {
        let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.get(id).map(|e| e.info.clone())
    }

    fn managed_spool_path(&self, id: &TaskId) -> Option<PathBuf> {
        let raw = id.as_str();
        let suffix = raw.strip_prefix("tsk_")?;
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return None;
        }
        Some(self.state.spool_dir.join(format!("{raw}.out")))
    }

    pub fn hydrate(&self, infos: Vec<TaskInfo>) {
        let evicted = {
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for info in infos {
                if info.status == TaskStatus::Running {
                    continue;
                }
                let remove_on_cleanup = self.managed_spool_path(&info.id).as_deref()
                    == Some(info.output_path.as_path());
                let spool = Arc::new(ManagedSpool::new(
                    info.output_path.clone(),
                    remove_on_cleanup,
                ));
                if let Some(entry) = tasks.get_mut(&info.id) {
                    if entry.info.status == TaskStatus::Running {
                        continue;
                    }
                    if entry.spool.path() != spool.path() {
                        let previous = std::mem::replace(&mut entry.spool, spool);
                        previous.request_cleanup();
                    }
                    entry.info = info;
                    entry.buffer = HeadTailBuffer::new(16 * 1024, self.state.config.tail_bytes);
                    entry.parser = ProgressParser::new();
                    entry.last_activity = std::time::Instant::now();
                    entry.stalled = false;
                    entry.kill_requested = false;
                    entry.child_pid = None;
                    entry.kill_tx.send_modify(|requested| *requested = false);
                    entry.supervisor = None;
                    entry.version_tx.send_modify(|version| *version += 1);
                    continue;
                }

                let (version_tx, _) = tokio::sync::watch::channel(0u64);
                let (kill_tx, _) = tokio::sync::watch::channel(false);
                tasks.insert(
                    info.id.clone(),
                    TaskEntry {
                        spool,
                        info,
                        call: None,
                        buffer: HeadTailBuffer::new(16 * 1024, self.state.config.tail_bytes),
                        parser: ProgressParser::new(),
                        version_tx,
                        last_activity: std::time::Instant::now(),
                        stalled: false,
                        kill_requested: false,
                        child_pid: None,
                        kill_tx,
                        supervisor: None,
                    },
                );
            }
            self.state.prune_terminal_overflow(&mut tasks)
        };
        drop(evicted);
    }

    pub fn reassign_owner(
        &self,
        session: &SessionId,
        from: &AgentId,
        to: &AgentId,
    ) -> Vec<TaskInfo> {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks
            .values_mut()
            .filter(|entry| entry.info.session == *session && entry.info.owner == *from)
            .map(|entry| {
                entry.info.owner = to.clone();
                entry.version_tx.send_modify(|version| *version += 1);
                entry.info.clone()
            })
            .collect()
    }

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

    pub fn forget(&self, id: &TaskId) {
        let removed = {
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if tasks
                .get(id)
                .is_some_and(|entry| entry.info.status.is_terminal())
            {
                tasks.remove(id)
            } else {
                None
            }
        };
        if let Some(entry) = removed {
            entry.spool.request_cleanup();
            drop(entry);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.state.events.subscribe()
    }
}

#[path = "output.rs"]
mod output;
#[path = "stop.rs"]
mod stop;

#[path = "foreground.rs"]
mod foreground;

#[cfg(test)]
#[path = "manager_tests.rs"]
mod manager_tests;
