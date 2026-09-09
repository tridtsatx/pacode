//! `Session`: one conversation with a main agent and its subagents.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_store::Store;
use pacode_tools::{AgentSpec, ToolRegistry};
use pacode_types::{
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
    pub tools: RwLock<ToolRegistry>,
    pub mcp: Arc<McpPool>,
    pub plugins: Arc<pacode_plugin::PluginHost>,
    pub app_version: String,
    pub skills: Arc<pacode_skills::SkillRegistry>,
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
        let has_running_agent = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .any(|agent| agent.is_running() || agent.status().is_active())
            })
            .unwrap_or(false);
        if has_running_agent {
            return true;
        }
        self.tasks_of_session()
            .iter()
            .any(|t| t.status == pacode_types::TaskStatus::Running)
    }

    /// Start a turn on the main agent, or queue a steer injection when a turn is
    /// running. Persists the user message. Returns immediately.
    pub async fn submit_user_message(self: &Arc<Self>, text: String) -> Result<(), CoreError> {
        let main = self
            .main_agent()
            .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;

        let (seq, arc_msg) = main
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(pacode_types::Message::user(&text));
        self.store
            .append_message(&self.id, &main.id, seq, &arc_msg)
            .await?;

        let item_seq = main
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = pacode_types::TranscriptItem {
            seq: item_seq,
            agent: main.id.clone(),
            ts_ms: pacode_types::now_ms(),
            kind: pacode_types::TranscriptKind::User { text: text.clone() },
        };
        main.transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(pacode_types::Event::ItemAdded(item));

        let needs_touch = {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            if meta.first_prompt.is_none() {
                meta.first_prompt = Some(text.clone());
                true
            } else {
                false
            }
        };
        if needs_touch {
            self.touch();
        }

        let _ = self
            .plugins
            .run_hooks(&pacode_plugin::HookEvent::OnMessage {
                role: "user".to_string(),
                text: text.clone(),
            })
            .await;

        if main.is_running() {
            main.injections
                .push(crate::inject::Injection::UserSteer(text));
        } else {
            crate::turn::start_turn(self.clone(), main);
        }

        Ok(())
    }

    /// Cancel the main agent's running turn (history kept).
    pub fn interrupt(&self) {
        if let Some(main) = self.main_agent()
            && let Some(cancel) = main
                .cancel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
        {
            cancel.cancel();
        }
    }

    pub fn resolve_permission(&self, id: &PermissionId, decision: PermissionDecision) -> bool {
        self.permissions.resolve(id, decision)
    }

    pub fn set_model(&self, route: ModelRoute) -> Result<(), CoreError> {
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.model = route;
        }
        self.touch();
        Ok(())
    }

    pub fn set_effort(&self, effort: Effort) {
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.effort = effort;
        }
        self.touch();
    }

    pub fn set_mode(&self, mode: Mode) {
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.mode = mode;
        }
        self.touch();
    }

    /// Spawn a subagent (depth 1, `agents.max_live` cap) and start its turn.
    pub async fn spawn_agent(self: &Arc<Self>, spec: AgentSpec) -> Result<AgentId, CoreError> {
        let live_count = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .filter(|agent| agent.status().is_live() && !agent.id.is_main())
                    .count()
            })
            .unwrap_or(0);
        if live_count >= self.config.agents.max_live {
            return Err(CoreError::Invalid(format!(
                "maximum live subagents ({}) reached",
                self.config.agents.max_live
            )));
        }

        let id = AgentId::generate();
        let name = spec.name.unwrap_or_else(|| {
            let count = self.agents.read().unwrap_or_else(|p| p.into_inner()).len();
            format!("agent-{count}")
        });

        let tools = if let Some(tool_names) = spec.tools {
            let filtered: Vec<&str> = tool_names
                .iter()
                .map(String::as_str)
                .filter(|n| *n != "agent")
                .collect();
            let g = self.tools.read().unwrap_or_else(|p| p.into_inner());
            g.subset(filtered)
        } else {
            let g = self.tools.read().unwrap_or_else(|p| p.into_inner());
            let default_names = pacode_tools::subagent_tool_names(&g);
            g.subset(default_names.iter().map(String::as_str))
        };

        let model = spec
            .model
            .or_else(|| {
                self.config
                    .agents
                    .default_model
                    .as_deref()
                    .and_then(|m| self.providers.parse_route(m))
            })
            .unwrap_or_else(|| self.meta().model);

        let effort = spec
            .effort
            .or(self.config.agents.default_effort)
            .unwrap_or_else(|| self.meta().effort);

        let mut history = crate::agent::History::default();
        if spec.fork
            && let Some(main) = self.main_agent()
        {
            let parent_hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
            history.messages = parent_hist.messages.clone();
            history.summary = parent_hist.summary.clone();
            history.summary_upto = parent_hist.summary_upto;
            history.estimated_tokens = parent_hist.estimated_tokens;
        }
        history.push(pacode_types::Message::user(&spec.prompt));

        let now = pacode_types::now_ms();
        let info = AgentInfo {
            id: id.clone(),
            name,
            kind: pacode_types::AgentKind::Sub,
            status: pacode_types::AgentStatus::Idle,
            activity: None,
            started_at_ms: now,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model,
            effort,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        };

        self.store
            .upsert_agent(&self.id, &info, Some(&spec.prompt))
            .await?;

        let agent = Arc::new(Agent {
            id: id.clone(),
            info: RwLock::new(info.clone()),
            history: std::sync::Mutex::new(history),
            injections: crate::inject::InjectionQueue::default(),
            tools: RwLock::new(tools),
            cancel: std::sync::Mutex::new(None),
            turn_lock: tokio::sync::Mutex::new(()),
            transcript: std::sync::Mutex::new(crate::transcript::TranscriptState::new(
                self.config.session.history_page as usize * 2,
            )),
            prompt: Some(spec.prompt),
            turns: std::sync::Mutex::new(0),
        });

        if let Ok(mut agents) = self.agents.write() {
            agents.insert(id.clone(), agent.clone());
        }

        self.events.emit(pacode_types::Event::AgentAdded(info));
        crate::turn::start_turn(self.clone(), agent);

        Ok(id)
    }

    pub async fn stop_agent(&self, id: &AgentId) -> Result<(), CoreError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| CoreError::AgentNotFound(id.clone()))?;
        if let Some(cancel) = agent
            .cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
        agent.set_status(pacode_types::AgentStatus::Stopped, None);
        let info = agent.info();
        let _ = self
            .store
            .upsert_agent(&self.id, &info, agent.prompt.as_deref())
            .await;
        self.events.emit(pacode_types::Event::AgentUpdated(info));
        Ok(())
    }

    pub fn set_plan(&self, plan: Plan) {
        if let Ok(mut p) = self.plan.write() {
            *p = plan.clone();
        }
        let store = self.store.clone();
        let session_id = self.id.clone();
        let plan_clone = plan.clone();
        tokio::spawn(async move {
            let _ = store.save_plan(&session_id, &plan_clone).await;
        });
        self.events.emit(pacode_types::Event::PlanUpdated(plan));
    }

    /// Everything a client needs to render from scratch (main agent transcript tail
    /// of `session.history_page` items).
    pub fn snapshot(&self) -> SessionSnapshot {
        let meta = self.meta();
        let agents = self.agent_infos();
        let plan = self.plan.read().unwrap_or_else(|p| p.into_inner()).clone();
        let tasks = self.tasks_of_session();
        let usage = self.usage.read().unwrap_or_else(|p| p.into_inner()).clone();
        let (transcript, has_more_history) = self
            .main_agent()
            .map(|m| m.transcript_tail(None, self.config.session.history_page as usize))
            .unwrap_or_default();
        let pending_permissions = self.permissions.pending();
        let turn_active = self.main_agent().map(|m| m.is_running()).unwrap_or(false);
        let seq = self.events.last_seq();

        SessionSnapshot {
            meta,
            agents,
            plan,
            tasks,
            usage,
            transcript,
            has_more_history,
            pending_permissions,
            turn_active,
            seq,
        }
    }

    /// Recompute `UsageTotals` after a turn and emit `UsageUpdated`.
    pub fn record_usage(&self, agent: &AgentId, usage: pacode_types::Usage, context_tokens: u32) {
        let model_name = self.meta().model.model;
        let pricing = self.config.pricing_for(&model_name);

        let totals = {
            let mut u = self.usage.write().unwrap_or_else(|p| p.into_inner());
            u.input = u.input.saturating_add(usage.input_tokens);
            u.output = u.output.saturating_add(usage.output_tokens);
            u.reasoning = u.reasoning.saturating_add(usage.reasoning_tokens);
            u.cache_read = u.cache_read.saturating_add(usage.cache_read_tokens);
            u.cache_write = u.cache_write.saturating_add(usage.cache_write_tokens);
            u.turns += 1;
            u.context_tokens = context_tokens;
            u.last_activity_ms = pacode_types::now_ms();
            if let Some(p) = pricing {
                let usage_struct = pacode_types::Usage {
                    input_tokens: u.input,
                    output_tokens: u.output,
                    reasoning_tokens: u.reasoning,
                    cache_read_tokens: u.cache_read,
                    cache_write_tokens: u.cache_write,
                };
                u.cost_usd = Some(p.cost_usd(&usage_struct));
            }
            u.clone()
        };

        if let Some(a) = self.agent(agent)
            && let Ok(mut info) = a.info.write()
        {
            info.tokens_in = info.tokens_in.saturating_add(usage.input_tokens);
            info.tokens_out = info.tokens_out.saturating_add(usage.output_tokens);
        }

        self.events.emit(pacode_types::Event::UsageUpdated(totals));
    }

    /// Persist meta (`updated_at` bumped) and emit `SessionUpdated`.
    pub fn touch(&self) {
        let meta = {
            let mut m = self.meta.write().unwrap_or_else(|p| p.into_inner());
            m.updated_at_ms = pacode_types::now_ms();
            m.clone()
        };
        let store = self.store.clone();
        let m_clone = meta.clone();
        tokio::spawn(async move {
            let _ = store.upsert_session(&m_clone).await;
        });
        self.events.emit(pacode_types::Event::SessionUpdated(meta));
    }
}
