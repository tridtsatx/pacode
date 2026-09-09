//! Task creation parameters.

use std::path::PathBuf;
use std::time::Duration;

use codeapp_types::{AgentId, SessionId};

#[derive(Clone, Debug, PartialEq)]
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
        }
    }
}
