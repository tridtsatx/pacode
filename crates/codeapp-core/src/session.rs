//! `Session`: one conversation with a main agent and its subagents.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use codeapp_exec::TaskManager;
use codeapp_mcp::McpPool;
use codeapp_provider::ProviderRegistry;
use codeapp_store::Store;
use codeapp_tools::{AgentSpec, ToolRegistry};
use codeapp_types::{
    AgentId, AgentInfo, Config, Effort, Mode, ModelRoute, PermissionDecision, PermissionId, Plan,
    SessionId, SessionMeta, SessionSnapshot, TaskInfo, UsageTotals,
};

use crate::CoreError;
use crate::agent::Agent;
use crate::permissions::PermissionState;
use crate::transcript::EventSink;

pub struct Session {
    pub id: SessionId,
    pub meta: RwLock<SessionMeta>,
    /// Main agent plus subagents, insertion order = spawn order.
    pub agents: RwLock<BTreeMap<AgentId, Arc<Agent>>>,
    pub plan: RwLock<Plan>,
    pub usage: RwLock<UsageTotals>,
    pub permissions: PermissionState,
    pub events: EventSink,
    pub tasks: Arc<TaskManager>,
    pub store: Store,
    pub config: Arc<Config>,
    pub providers: Arc<ProviderRegistry>,
    /// Built-in + MCP tools available in this session (subagents get subsets).
    pub tools: ToolRegistry,
    pub mcp: Arc<McpPool>,
    pub app_version: String,
}

impl Session {
    pub fn main_agent(&self) -> Option<Arc<Agent>> {
        self.agents.read().ok()?.get(&AgentId::main()).cloned()
    }

    pub fn agent(&self, id: &AgentId) -> Option<Arc<Agent>> {
        self.agents.read().ok()?.get(id).cloned()
    }

    pub fn agent_infos(&self) -> Vec<AgentInfo> {
        self.agents
            .read()
            .map(|a| a.values().map(|agent| agent.info()).collect())
            .unwrap_or_default()
    }

    pub fn meta(&self) -> SessionMeta {
        self.meta
            .read()
            .map(|m| m.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn tasks_of_session(&self) -> Vec<TaskInfo> {
        self.tasks.list(Some(&self.id))
    }

    /// Any live subagent, running turn, or running task.
    pub fn is_busy(&self) -> bool {
        todo!("Session::is_busy")
    }

    /// Start a turn on the main agent, or queue a steer injection when a turn is
    /// running. Persists the user message. Returns immediately.
    pub async fn submit_user_message(self: &Arc<Self>, text: String) -> Result<(), CoreError> {
        let _ = text;
        todo!("Session::submit_user_message")
    }

    /// Cancel the main agent's running turn (history kept).
    pub fn interrupt(&self) {
        todo!("Session::interrupt")
    }

    pub fn resolve_permission(&self, id: &PermissionId, decision: PermissionDecision) -> bool {
        let _ = (id, decision);
        todo!("Session::resolve_permission")
    }

    pub fn set_model(&self, route: ModelRoute) -> Result<(), CoreError> {
        let _ = route;
        todo!("Session::set_model")
    }

    pub fn set_effort(&self, effort: Effort) {
        let _ = effort;
        todo!("Session::set_effort")
    }

    pub fn set_mode(&self, mode: Mode) {
        let _ = mode;
        todo!("Session::set_mode")
    }

    /// Spawn a subagent (depth 1, `agents.max_live` cap) and start its turn.
    pub async fn spawn_agent(self: &Arc<Self>, spec: AgentSpec) -> Result<AgentId, CoreError> {
        let _ = spec;
        todo!("Session::spawn_agent")
    }

    pub async fn stop_agent(&self, id: &AgentId) -> Result<(), CoreError> {
        let _ = id;
        todo!("Session::stop_agent")
    }

    pub fn set_plan(&self, plan: Plan) {
        let _ = plan;
        todo!("Session::set_plan")
    }

    /// Everything a client needs to render from scratch (main agent transcript tail
    /// of `session.history_page` items).
    pub fn snapshot(&self) -> SessionSnapshot {
        todo!("Session::snapshot")
    }

    /// Recompute `UsageTotals` after a turn and emit `UsageUpdated`.
    pub fn record_usage(&self, agent: &AgentId, usage: codeapp_types::Usage, context_tokens: u32) {
        let _ = (agent, usage, context_tokens);
        todo!("Session::record_usage")
    }

    /// Persist meta (`updated_at` bumped) and emit `SessionUpdated`.
    pub fn touch(&self) {
        todo!("Session::touch")
    }
}
