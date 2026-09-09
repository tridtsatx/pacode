//! `ToolHost`: the only interface tools use to reach the core (permissions, background
//! tasks, subagents, plan, UI previews). The core implements it; tests use a stub.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::TaskSpec;
use pacode_types::{
    AgentId, AgentInfo, CallId, Effort, Mode, ModelRoute, PermissionDecision, Plan, RiskLevel,
    SessionId, TaskId, TaskInfo, TaskProgress,
};
use tokio_util::sync::CancellationToken;

use crate::output::ToolError;

/// Per-call context handed to [`crate::Tool::call`].
#[derive(Clone)]
pub struct ToolCtx {
    pub session: SessionId,
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    pub cwd: PathBuf,
    pub mode: Mode,
    pub host: Arc<dyn ToolHost>,
    pub cancel: CancellationToken,
    /// From `[context].tool_output_cap_chars`.
    pub output_cap_chars: usize,
    /// From `[exec].yield_after_secs`: foreground wait before a command is backgrounded.
    pub exec_yield_after: Duration,
    /// From `[exec].default_timeout_secs`.
    pub exec_default_timeout: Duration,
}

impl ToolCtx {
    /// Resolve a model-supplied path against the working directory.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// Ask for permission with a one-line title and a detail block. Returns `Err(Denied)`
    /// when refused, so tools can `?` it.
    pub async fn require_permission(
        &self,
        title: impl Into<String>,
        detail: impl Into<String>,
        risk: Option<RiskLevel>,
    ) -> Result<PermissionDecision, ToolError> {
        let decision = self
            .host
            .request_permission(PermissionDraft {
                agent: self.agent.clone(),
                agent_name: self.agent_name.clone(),
                call_id: self.call_id.clone(),
                title: title.into(),
                detail: detail.into(),
                risk,
            })
            .await;
        match decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowSession => Ok(decision),
            PermissionDecision::Deny => Err(ToolError::Denied("user denied the request".into())),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionDraft {
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    pub title: String,
    pub detail: String,
    pub risk: Option<RiskLevel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<ModelRoute>,
    pub effort: Option<Effort>,
    /// `true` = copy of the parent's history at spawn time.
    pub fork: bool,
    /// Tool names allowed for the subagent; `None` = the default subagent set.
    pub tools: Option<Vec<String>>,
}

/// Result of waiting on tasks or agents.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitOutcome {
    Finished,
    Timeout,
    Progress,
    Cancelled,
}

/// Everything a tool may ask the core for. Implemented by `pacode-core`.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Ask the user (or the mode) whether the call may proceed. The host applies the
    /// permission matrix and the `AllowSession` cache before prompting.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionDecision;

    // --- background tasks (owner = session) ---
    async fn spawn_task(&self, spec: TaskSpec) -> Result<TaskId, ToolError>;
    fn task_info(&self, task: &TaskId) -> Option<TaskInfo>;
    fn list_tasks(&self) -> Vec<TaskInfo>;
    /// Wait until the task ends, `timeout` elapses, or (when `return_on_progress`) its
    /// progress changes.
    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome;
    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError>;
    /// Last `lines` lines of the spooled output.
    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError>;
    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError>;

    // --- subagents (depth 1) ---
    async fn spawn_agent(&self, spec: AgentSpec) -> Result<AgentId, ToolError>;
    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo>;
    fn list_agents(&self) -> Vec<AgentInfo>;
    async fn wait_agent(&self, agent: &AgentId, timeout: Duration) -> WaitOutcome;
    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Orchestrator → subagent: ask for a brief status. Delivered to the subagent as a
    /// `<status_request>` injection at its next step; it answers with `report_status`.
    fn request_agent_status(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Subagent → parent: a short status line, delivered as `<agent_status>` at the
    /// parent's next step.
    fn report_status(&self, text: String) -> Result<(), ToolError>;

    // --- plan ---
    fn plan(&self) -> Plan;
    fn set_plan(&self, plan: Plan);

    // --- UI ---
    /// Stream a bounded preview of in-progress output to the transcript row.
    fn emit_preview(&self, call_id: &CallId, preview: String);
}
