//! Task creation parameters.

use std::path::PathBuf;
use std::time::Duration;

use pacode_types::{AgentId, CallId, SessionId};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct TaskSpec {
    pub session: SessionId,
    pub owner: AgentId,
    /// Run through `sh -c` (or `$SHELL -c` when set).
    pub command: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Rail label; derived from the command when `None`.
    pub label: Option<String>,
    /// Started with `background: true` (no foreground wait at all).
    pub background: bool,
    /// Hard timeout; the task is killed and marked failed when it elapses.
    pub timeout: Option<Duration>,
    /// Opts into the admission queue: with a token, a spawn that finds every
    /// `exec.max_tasks` slot busy waits (FIFO) for one, and the token cancels that
    /// wait; with `None` such a spawn fails at once with `ExecError::TooManyTasks`.
    /// Only the wait is cancelled: a started task is stopped with `kill`.
    pub cancel: Option<CancellationToken>,
    /// The tool call waiting on this task in the foreground, whose transcript
    /// row its live output belongs to. `None`: nobody draws it live.
    pub call: Option<CallId>,
    /// For a foreground task: if the call waiting on it is dropped while it
    /// runs, kill it instead of moving it to the background — a read whose
    /// output only that call wanted (see `ForegroundGuard`).
    pub kill_if_abandoned: bool,
}

impl TaskSpec {
    pub fn new(
        session: SessionId,
        owner: AgentId,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            session,
            owner,
            command: command.into(),
            cwd: cwd.into(),
            env: Vec::new(),
            label: None,
            background: false,
            timeout: None,
            cancel: None,
            call: None,
            kill_if_abandoned: false,
        }
    }
}
