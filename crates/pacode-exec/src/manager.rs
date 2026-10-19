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
//! - events on a `tokio::sync::broadcast` channel (capacity 1024, lagging receivers just
//!   miss events and should re-list);
//! - `shutdown` kills, escalates to SIGKILL, waits for the supervisors, and marks
//!   whatever is left as killed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacode_types::{
    AgentId, ExecConfig, ProgressSource, ResourcesConfig, SessionId, TaskId, TaskInfo,
    TaskProgress, TaskStatus,
};
use tokio::sync::broadcast;

use crate::admission::{Admission, SlotPermit};
use crate::buffer::HeadTailBuffer;
use crate::memcap::MemoryPolicy;
use crate::memlimit::{MemoryLimiter, SpawnSetup};
use crate::progress::ProgressParser;
use crate::resources::{GuardHost, ResourceGuard, ResourceNotice};
use crate::spec::TaskSpec;
use crate::spool::SpoolGc;
use crate::state::{ManagedSpool, SharedState, TaskEntry};

#[derive(Clone, Debug, PartialEq)]
pub enum TaskEvent {
    Started(TaskInfo),
    Progress(TaskInfo),
    Ended(TaskInfo),
    /// A foreground task ended, but the call waiting on it was dropped before
    /// its result was delivered (see [`crate::ForegroundGuard`]): nothing told
    /// the model, so it is announced like a background end. Follows `Ended`.
    Unreported(TaskInfo),
    /// No output and no progress for `exec.stall_secs`; the task is still running.
    Stalled(TaskInfo),
    /// The resource guard entered a warning/breach level or recovered.
    Resource(ResourceNotice),
    /// Output the process produced, on whole lines, while it is still running.
    ///
    /// One event per batch of completed lines, not per line and not per read:
    /// the lines of every read inside a 200 ms window (or up to 64 KiB) go out
    /// together, so a build that flushes 8 KiB at a time does not put a hundred
    /// events on the wire to say one thing. Nothing is dropped: held text is sent
    /// when the window closes, and at the latest just before `Ended`. A partial
    /// trailing line waits for its newline — a client that drew half a line
    /// would repaint it a moment later anyway.
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
    /// The admission queue itself is full (carries the queue cap).
    #[error("too many tasks ({0})")]
    TooManyTasks(usize),
    /// The wait for a free process slot was cancelled; nothing was started.
    #[error("cancelled while waiting for a process slot")]
    Cancelled,
    #[error("spawn failed: {0}")]
    Spawn(std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Machine memory or spool disk is low; carries the full user-facing text.
    #[error("{0}")]
    LowResources(String),
    #[error("tool memory limit: {0}")]
    MemoryLimit(#[source] std::io::Error),
}

/// Capacity of the task-event broadcast channel (owner: exec; cap: this constant;
/// invalidation: none, fixed at construction). Sized for bursts of task events (F6);
/// a `Lagged` receiver is handled by the router, the capacity keeps that path rare.
pub(crate) const TASK_EVENT_CAPACITY: usize = 1024;

pub struct TaskManager {
    pub(crate) state: Arc<SharedState>,
    /// The machine-level memory policy every spawn of this daemon goes
    /// through. One per daemon, shared with the tools so that a raise is
    /// visible to every later spawn.
    memory: MemoryLimiter,
}

/// `[resources]` with the guard and the niceness off (`new`, `with_memory`).
fn disabled_resources() -> ResourcesConfig {
    ResourcesConfig {
        enabled: false,
        nice: 0,
        ..ResourcesConfig::default()
    }
}

impl TaskManager {
    /// Build a manager with no tool memory cap and no resource guard.
    ///
    /// The daemon does not use this; it supplies the user's
    /// `ui.tool_memory_percent` and `ui.tool_memory_split_sessions` and its
    /// `[resources]` through [`TaskManager::with_limits`]. A caller that reaches
    /// for `new` without a policy gets an uncapped manager rather than a guessed
    /// one — a cap derived from a default nobody chose is a cap nobody can debug.
    pub fn new(spool_dir: PathBuf, config: ExecConfig) -> Arc<Self> {
        Self::with_memory(
            spool_dir,
            config,
            MemoryLimiter::new(MemoryPolicy::disabled()),
        )
    }

    /// Build a manager that caps what a spawned process may use (no resource guard).
    pub fn with_memory(spool_dir: PathBuf, config: ExecConfig, memory: MemoryLimiter) -> Arc<Self> {
        Self::with_limits(spool_dir, config, disabled_resources(), memory)
    }

    /// Build a manager with the machine-resource guard (no tool memory cap).
    pub fn with_resources(
        spool_dir: PathBuf,
        config: ExecConfig,
        resources: ResourcesConfig,
    ) -> Arc<Self> {
        Self::with_limits(
            spool_dir,
            config,
            resources,
            MemoryLimiter::new(MemoryPolicy::disabled()),
        )
    }

    /// Build a manager with both the resource guard and the tool memory cap.
    pub fn with_limits(
        spool_dir: PathBuf,
        config: ExecConfig,
        resources: ResourcesConfig,
        memory: MemoryLimiter,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(TASK_EVENT_CAPACITY);
        let guard = ResourceGuard::new(&resources, spool_dir.clone());
        let admission = Arc::new(Admission::new(config.max_tasks));
        let state = Arc::new(SharedState {
            spool_dir,
            config,
            tasks: Mutex::new(HashMap::new()),
            events,
            guard,
            nice: resources.nice,
            admission,
            spool_gc: SpoolGc::default(),
        });
        let host: Arc<dyn GuardHost> = state.clone();
        state.guard.bind(Arc::downgrade(&host));
        // F5: sweep spools left by earlier runs.
        crate::spool::gc_at_start(&state);
        Arc::new(Self { state, memory })
    }

    /// The memory policy: ceilings, the live-session division, and raises.
    pub fn memory(&self) -> &MemoryLimiter {
        &self.memory
    }

    /// Refuse a new subagent when machine resources are low.
    pub fn admit_agent(&self) -> Result<(), ExecError> {
        self.state.guard.admit_agent()
    }

    pub fn guard(&self) -> &ResourceGuard {
        &self.state.guard
    }

    /// Exempt (or re-include) a task tree from resource-guard kills.
    pub fn set_protected(&self, id: &TaskId, protected: bool) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.protected = protected;
        }
    }

    /// One of the `exec.max_tasks` process slots (F4). With `spec.cancel` the
    /// caller waits for it in FIFO order (cancellable); without, a busy manager
    /// refuses at once with `TooManyTasks(max_tasks)`.
    async fn admit(&self, spec: &TaskSpec) -> Result<SlotPermit, ExecError> {
        let permit = match spec.cancel.as_ref() {
            Some(cancel) => self.state.admission.acquire(Some(cancel)).await?,
            None => match self.state.admission.try_acquire() {
                Some(permit) => permit,
                None => return Err(ExecError::TooManyTasks(self.state.config.max_tasks)),
            },
        };
        self.state.guard.admit()?;
        Ok(permit)
    }

    /// Start the process. Returns immediately with the initial `TaskInfo`.
    pub async fn spawn(&self, mut spec: TaskSpec) -> Result<TaskInfo, ExecError> {
        tokio::fs::create_dir_all(&self.state.spool_dir)
            .await
            .map_err(ExecError::Spawn)?;

        let permit = self.admit(&spec).await?;
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
        #[cfg(unix)]
        {
            let nice = self.state.nice.clamp(0, 19);
            if nice > 0 || plan.setup != SpawnSetup::None {
                let setup = plan.setup.clone();
                // SAFETY: only async-signal-safe syscalls run between fork and exec:
                // setpriority(2), and `SpawnSetup::install`, which is documented and
                // implemented as bare syscalls on memory allocated before the fork —
                // no allocator, no locks, no Rust runtime state.
                unsafe {
                    cmd.pre_exec(move || {
                        if nice > 0 {
                            libc::setpriority(libc::PRIO_PROCESS, 0, nice);
                        }
                        setup.install()
                    });
                }
            }
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
            started: std::time::Instant::now(),
            protected: false,
            guard_reason: None,
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
                permit,
            )
            .await;
        });
        entry.supervisor = Some(supervisor);

        {
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            tasks.insert(entry.info.id.clone(), entry);
        }

        let _ = self.state.events.send(TaskEvent::Started(info.clone()));
        // Before the supervisor can finish: starts sampling on 0 -> 1.
        self.state.guard.process_started();
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
                        started: std::time::Instant::now(),
                        protected: false,
                        guard_reason: None,
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

    /// Running tasks and the newest finished ones (`MAX_TERMINAL_TASKS` in
    /// `state.rs`), optionally for one session, ordered by start time.
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

    /// Clear the failed-unacked flag.
    pub fn ack(&self, id: &TaskId) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.info.acked = true;
        }
    }

    /// Drop a finished task from the list; its spool file is removed with it.
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
pub use foreground::ForegroundGuard;

#[cfg(test)]
#[path = "manager_tests.rs"]
mod manager_tests;
