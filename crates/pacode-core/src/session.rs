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
    /// Questions the model asked and is waiting on.
    pub questions: crate::questions::QuestionState,
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
    /// Cron jobs and monitors of this session. Dropping the session stops them.
    pub scheduler: Arc<crate::schedule::Scheduler>,
}

impl Session {
    /// Record the answer on the question's transcript item, so the conversation
    /// shows what was asked and what was chosen rather than only the tool result.
    pub fn record_question_answer(
        &self,
        question: &pacode_types::Question,
        answer: &pacode_types::QuestionAnswer,
    ) {
        let Some(agent) = self.agent(&question.agent) else {
            return;
        };
        let seq = {
            let transcript = agent.transcript.lock().unwrap_or_else(|p| p.into_inner());
            transcript.find_question_seq(&question.id)
        };
        let Some(seq) = seq else {
            return;
        };
        let item = pacode_types::TranscriptItem {
            seq,
            agent: question.agent.clone(),
            ts_ms: pacode_types::time::now_ms(),
            kind: pacode_types::TranscriptKind::Question {
                question: question.clone(),
                answer: Some(answer.clone()),
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(pacode_types::Event::ItemUpdated(item));
    }

    /// Tell the main agent that a monitor it started has fired. Injections reach
    /// the model between steps, never mid-stream (spec §6.2).
    pub fn inject_monitor_fired(&self, monitor: &pacode_types::MonitorInfo) {
        let Some(main) = self.main_agent() else {
            return;
        };
        let text = format!(
            "Monitor {} fired: {}",
            monitor.label,
            monitor.condition.describe()
        );
        main.injections
            .push(crate::inject::Injection::SystemNotice(text));
    }

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

        let cwd = self.meta().cwd;
        let expanded_text = crate::expand::expand_user_message(
            &text,
            &cwd,
            self.config.context.tool_output_cap_chars,
        );

        if !main.is_running() {
            let hist_snapshot = main
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            *main
                .history_before_turn
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(hist_snapshot);
        }

        let (seq, arc_msg) = main
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(pacode_types::Message::user(&expanded_text));
        self.store
            .append_message(&self.id, &main.id(), seq, &arc_msg)
            .await?;

        let item_seq = main
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = pacode_types::TranscriptItem {
            seq: item_seq,
            agent: main.id(),
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
        // A question the turn is blocked on has to go with the turn, or the tool
        // call keeps waiting for an answer nobody will give now.
        self.cancel_questions_for(&AgentId::main());
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

    /// Dismiss every question an agent is waiting on, telling clients to drop
    /// their pickers.
    pub fn cancel_questions_for(&self, agent: &AgentId) {
        for question in self.questions.cancel_for_agent(agent) {
            let answer = pacode_types::QuestionAnswer::cancelled();
            self.record_question_answer(&question, &answer);
            self.events.emit(pacode_types::Event::QuestionResolved {
                question: question.id,
                answer,
            });
        }
    }

    pub fn resolve_permission(&self, id: &PermissionId, decision: PermissionDecision) -> bool {
        self.permissions.resolve(id, decision)
    }

    /// Switch the session model. The route must resolve against this session's
    /// provider registry — the turn loop resolves the same way, so an unknown
    /// provider is rejected here instead of failing the next turn.
    ///
    /// The route propagates to the main agent's `AgentInfo` and to every live
    /// subagent still on the old session model: those inherited it via
    /// `spec.model = None`, while subagents spawned with an explicit model keep
    /// theirs. Each updated info is persisted and broadcast, so the next loop
    /// iteration of a running turn sees the new route too.
    pub async fn set_model(&self, route: ModelRoute) -> Result<(), CoreError> {
        self.providers.resolve(&route).await?;

        let old_model = {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut meta.model, route.clone())
        };

        self.update_agent_infos(|info| {
            if info.id.is_main() || (info.status.is_live() && info.model == old_model) {
                info.model = route.clone();
                true
            } else {
                false
            }
        })
        .await;

        self.touch();
        Ok(())
    }

    /// Same propagation as `set_model`, for the reasoning effort.
    pub async fn set_effort(&self, effort: Effort) {
        let old_effort = {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut meta.effort, effort)
        };

        self.update_agent_infos(|info| {
            if info.id.is_main() || (info.status.is_live() && info.effort == old_effort) {
                info.effort = effort;
                true
            } else {
                false
            }
        })
        .await;

        self.touch();
    }

    pub fn set_mode(&self, mode: Mode) {
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.mode = mode;
        }
        self.touch();
    }

    /// Run `update` against each agent's `AgentInfo`; where it returns `true`,
    /// persist the row and emit `AgentUpdated`.
    async fn update_agent_infos(&self, update: impl Fn(&mut AgentInfo) -> bool) {
        let agents: Vec<Arc<Agent>> = self
            .agents
            .read()
            .map(|a| a.values().cloned().collect())
            .unwrap_or_default();
        for agent in agents {
            let updated_info = {
                let mut info = agent.info.write().unwrap_or_else(|p| p.into_inner());
                if update(&mut info) {
                    Some(info.clone())
                } else {
                    None
                }
            };
            let Some(info) = updated_info else {
                continue;
            };
            if let Err(e) = self
                .store
                .upsert_agent(&self.id, &info, agent.prompt().as_deref())
                .await
            {
                log::warn!(
                    "failed to persist agent {} in session {}: {e}",
                    info.id,
                    self.id
                );
            }
            self.events.emit(pacode_types::Event::AgentUpdated(info));
        }
    }

    /// Spawn a subagent (depth 1, `agents.max_live` cap) and start its turn.
    pub async fn spawn_agent(self: &Arc<Self>, spec: AgentSpec) -> Result<AgentId, CoreError> {
        let live_count = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .filter(|agent| agent.status().is_live() && !agent.id().is_main())
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

        let agent = Arc::new(Agent::new(
            id.clone(),
            info.clone(),
            history,
            tools,
            self.config.session.history_page,
            Some(spec.prompt),
        ));

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
        self.cancel_questions_for(id);
        agent.set_status(pacode_types::AgentStatus::Stopped, None);
        let info = agent.info();
        let _ = self
            .store
            .upsert_agent(&self.id, &info, agent.prompt().as_deref())
            .await;
        self.events.emit(pacode_types::Event::AgentUpdated(info));
        Ok(())
    }

    /// Detach a running turn on the main agent into a background subagent.
    pub async fn detach_main_turn(self: &Arc<Self>) -> Result<AgentId, CoreError> {
        log::info!(
            "detaching running turn on main agent in session {}",
            self.id
        );
        let live_count = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .filter(|agent| agent.status().is_live() && !agent.id().is_main())
                    .count()
            })
            .unwrap_or(0);
        if live_count >= self.config.agents.max_live {
            log::warn!(
                "failed to detach main turn: maximum live subagents ({}) reached",
                self.config.agents.max_live
            );
            return Err(CoreError::Invalid(format!(
                "maximum live subagents ({}) reached",
                self.config.agents.max_live
            )));
        }

        let main = self
            .main_agent()
            .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;

        if !main.is_running() {
            log::warn!("failed to detach main turn: no turn is running on the main agent");
            return Err(CoreError::Invalid(
                "no turn is running on the main agent".to_string(),
            ));
        }

        let history_before = main
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();

        let prior_history = history_before.unwrap_or_else(|| {
            let hist_guard = main.history.lock().unwrap_or_else(|p| p.into_inner());
            let mut msgs = hist_guard.messages.clone();
            if !msgs.is_empty() {
                msgs.pop();
            }
            let est_tokens = msgs.iter().map(|m| m.estimate_tokens()).sum::<u32>()
                + hist_guard
                    .summary
                    .as_deref()
                    .map(pacode_types::estimate_tokens)
                    .unwrap_or(0);
            crate::agent::History {
                messages: msgs,
                next_seq: hist_guard.next_seq.saturating_sub(1),
                summary: hist_guard.summary.clone(),
                summary_upto: hist_guard.summary_upto,
                estimated_tokens: est_tokens,
            }
        });

        let turn_prompt = {
            let hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
            hist.messages
                .iter()
                .rev()
                .find(|m| m.role == pacode_types::Role::User)
                .map(|m| m.text())
        };
        main.set_prompt(turn_prompt.clone());

        let new_id = AgentId::generate();
        let name = {
            let count = self.agents.read().map(|a| a.len()).unwrap_or(1);
            format!("agent-{count}")
        };

        let detached_info = {
            let mut agents_guard = self.agents.write().unwrap_or_else(|p| p.into_inner());

            // Lock the running agent's transcript while swapping identity
            let mut transcript = main.transcript.lock().unwrap_or_else(|p| p.into_inner());

            // 1. Swap ID atomically
            main.set_id(new_id.clone());

            // 2. Restrict tools to subagent subset (exclude `agent` tool)
            {
                let g = self.tools.read().unwrap_or_else(|p| p.into_inner());
                let default_names = pacode_tools::subagent_tool_names(&g);
                let sub_tools = g.subset(default_names.iter().map(String::as_str));
                if let Ok(mut t) = main.tools.write() {
                    *t = sub_tools;
                }
            }

            // 3. Update info to subagent
            let detached_info = {
                let mut info_guard = main.info.write().unwrap_or_else(|p| p.into_inner());
                info_guard.id = new_id.clone();
                info_guard.name = name;
                info_guard.kind = pacode_types::AgentKind::Sub;
                info_guard.parent = Some(AgentId::main());
                info_guard.clone()
            };

            // 4. Retag all transcript items in the detached agent's transcript
            for item in &mut transcript.tail {
                item.agent = new_id.clone();
            }
            for item in transcript.tool_items.values_mut() {
                item.agent = new_id.clone();
            }
            drop(transcript);

            // 5. Re-register running agent under new_id
            agents_guard.insert(new_id.clone(), main.clone());

            // 6. Create FRESH main agent
            let main_tools = self.tools.read().unwrap_or_else(|p| p.into_inner()).clone();
            let now = pacode_types::now_ms();
            let fresh_main_info = pacode_types::AgentInfo {
                id: AgentId::main(),
                name: "main".to_string(),
                kind: pacode_types::AgentKind::Main,
                status: pacode_types::AgentStatus::Idle,
                activity: None,
                started_at_ms: now,
                finished_at_ms: None,
                tokens_in: 0,
                tokens_out: 0,
                model: self.meta().model,
                effort: self.meta().effort,
                parent: None,
                summary: None,
                error: None,
            };

            let mut fresh_transcript = crate::transcript::TranscriptState::new(
                self.config.session.history_page as usize * 2,
            );
            let prior_tuples: Vec<(u64, Arc<pacode_types::Message>)> = prior_history
                .messages
                .iter()
                .enumerate()
                .map(|(i, m)| (i as u64, m.clone()))
                .collect();
            let items = crate::transcript::history_to_items(&AgentId::main(), &prior_tuples, 0);
            for item in items {
                fresh_transcript.upsert(item);
            }

            let fresh_main = Arc::new(Agent::new_with_transcript(
                AgentId::main(),
                fresh_main_info.clone(),
                prior_history,
                main_tools,
                fresh_transcript,
                None,
            ));

            agents_guard.insert(AgentId::main(), fresh_main);

            detached_info
        };

        // 7. Store updates
        self.store
            .upsert_agent(&self.id, &detached_info, turn_prompt.as_deref())
            .await?;

        {
            let msgs = main
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .messages
                .clone();
            for (seq, msg) in msgs.iter().enumerate() {
                let _ = self
                    .store
                    .append_message(&self.id, &new_id, seq as u64, msg)
                    .await;
            }
        }

        // 8. Emit events
        self.events
            .emit(pacode_types::Event::AgentAdded(detached_info));
        if let Some(fresh_main) = self.main_agent() {
            self.events
                .emit(pacode_types::Event::AgentUpdated(fresh_main.info()));
        }

        log::info!(
            "detached running turn on main agent into background subagent {new_id} in session {}",
            self.id
        );

        Ok(new_id)
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
            pending_questions: self.questions.pending(),
            cron_jobs: self.scheduler.jobs(),
            monitors: self.scheduler.monitors(),
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

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
