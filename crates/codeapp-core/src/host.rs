//! `ToolHost` implementation: the session as seen by tools.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use codeapp_exec::TaskSpec;
use codeapp_tools::host::PermissionDraft;
use codeapp_tools::{AgentSpec, ToolError, ToolHost, WaitOutcome};
use codeapp_types::{
    AgentId, AgentInfo, CallId, PermissionDecision, Plan, TaskId, TaskInfo, TaskProgress,
};

use crate::session::Session;

pub struct SessionHost {
    pub session: Arc<Session>,
    /// The agent making the calls (its mode/tools decide the gate).
    pub agent: AgentId,
}

#[async_trait]
impl ToolHost for SessionHost {
    /// Applies `permissions::gate` with the session mode and the draft's risk; checks
    /// the AllowSession cache; otherwise registers the request, emits
    /// `PermissionRequested` + an `ItemAdded(Permission)` item, sets the agent to
    /// `WaitingApproval`, awaits the decision (cancel → Deny), emits
    /// `PermissionResolved`, caches `AllowSession`.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionDecision {
        let _ = draft;
        todo!("SessionHost::request_permission")
    }

    async fn spawn_task(&self, spec: TaskSpec) -> Result<TaskId, ToolError> {
        let _ = spec;
        todo!("SessionHost::spawn_task")
    }

    fn task_info(&self, task: &TaskId) -> Option<TaskInfo> {
        self.session.tasks.info(task)
    }

    fn list_tasks(&self) -> Vec<TaskInfo> {
        self.session.tasks_of_session()
    }

    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome {
        let _ = (task, timeout, return_on_progress);
        todo!("SessionHost::wait_task")
    }

    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError> {
        let _ = task;
        todo!("SessionHost::kill_task")
    }

    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError> {
        let _ = (task, lines);
        todo!("SessionHost::task_tail")
    }

    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError> {
        let _ = (task, progress);
        todo!("SessionHost::report_task_progress")
    }

    async fn spawn_agent(&self, spec: AgentSpec) -> Result<AgentId, ToolError> {
        let _ = spec;
        todo!("SessionHost::spawn_agent")
    }

    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo> {
        self.session.agent(agent).map(|a| a.info())
    }

    fn list_agents(&self) -> Vec<AgentInfo> {
        self.session.agent_infos()
    }

    async fn wait_agent(&self, agent: &AgentId, timeout: Duration) -> WaitOutcome {
        let _ = (agent, timeout);
        todo!("SessionHost::wait_agent")
    }

    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError> {
        let _ = agent;
        todo!("SessionHost::stop_agent")
    }

    fn plan(&self) -> Plan {
        self.session
            .plan
            .read()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    fn set_plan(&self, plan: Plan) {
        self.session.set_plan(plan);
    }

    fn emit_preview(&self, call_id: &CallId, preview: String) {
        let _ = (call_id, preview);
        todo!("SessionHost::emit_preview")
    }
}
