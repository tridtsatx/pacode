//! `Core`: the daemon-facing API.

pub(crate) mod global;
pub(crate) mod open;
pub(crate) mod router;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_store::Store;
use pacode_tools::{Tool, ToolRegistry};
use pacode_types::{Attach, Config, Event, Reply, Request, SessionId, SessionSnapshot};
use tokio::sync::broadcast;

use crate::CoreError;
use crate::session::Session;

/// Everything the core needs, built by the daemon.
pub struct CoreDeps {
    pub config: Arc<Config>,
    pub paths: Paths,
    pub providers: Arc<ProviderRegistry>,
    /// Built-in tools; MCP tools are appended per session from `mcp`.
    pub tools: ToolRegistry,
    pub tasks: Arc<TaskManager>,
    pub mcp: Arc<McpPool>,
    pub plugins: Arc<pacode_plugin::PluginHost>,
    /// Reads plugin marketplaces and installs from them.
    pub marketplace: Arc<pacode_plugin::marketplace::Marketplace>,
    pub store: Store,
    /// Shown in the rail anchor and in `SessionMeta` logs.
    pub app_version: String,
    pub skills: Arc<pacode_skills::SkillRegistry>,
}

pub struct Core {
    pub(crate) deps: CoreDeps,
    /// Live provider registry; replaced by `reload_config` (new sessions use it).
    pub(crate) providers: RwLock<Arc<ProviderRegistry>>,
    pub(crate) config: RwLock<Arc<Config>>,
    pub(crate) auth_driver: RwLock<Arc<dyn crate::auth::AuthDriver>>,
    pub(crate) sessions: RwLock<BTreeMap<SessionId, Arc<Session>>>,
    pub(crate) self_weak: std::sync::OnceLock<std::sync::Weak<Core>>,
    /// Routes `TaskEvent`s to the owning session (injections + UI events).
    pub(crate) task_router: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) cached_mcp_tools: std::sync::Mutex<Option<Vec<Arc<dyn Tool>>>>,
}

impl Core {
    /// Build the core and start the task-event router.
    pub async fn new(deps: CoreDeps) -> Arc<Core> {
        let rx = deps.tasks.subscribe();
        let providers = RwLock::new(deps.providers.clone());
        let config = RwLock::new(deps.config.clone());
        let auth_driver: RwLock<Arc<dyn crate::auth::AuthDriver>> =
            RwLock::new(Arc::new(crate::auth::DefaultAuthDriver));
        let core = Arc::new(Core {
            deps,
            providers,
            config,
            auth_driver,
            sessions: RwLock::new(BTreeMap::new()),
            self_weak: std::sync::OnceLock::new(),
            task_router: std::sync::Mutex::new(None),
            cached_mcp_tools: std::sync::Mutex::new(None),
        });

        let core_weak = Arc::downgrade(&core);
        let _ = core.self_weak.set(core_weak.clone());
        let handle = router::start_task_router(core_weak, rx);
        *core.task_router.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
        core
    }

    pub(crate) async fn get_mcp_tools(&self) -> Vec<Arc<dyn Tool>> {
        if self.deps.mcp.server_names().is_empty() {
            return Vec::new();
        }
        if let Ok(guard) = self.cached_mcp_tools.lock()
            && let Some(tools) = guard.as_ref()
        {
            return tools.clone();
        }
        let tools = pacode_tools::builtin::mcp::mcp_tools(self.deps.mcp.clone()).await;
        if let Ok(mut guard) = self.cached_mcp_tools.lock() {
            *guard = Some(tools.clone());
        }
        tools
    }

    /// `Attach::New` creates a session (persisted immediately); `Resume` loads meta,
    /// plan, agents (as finished), tasks (as finished/killed) and the main agent's
    /// history (compaction summary + messages after it) from the store; `Latest`
    /// resumes the most recently updated session for `cwd` or creates one.
    pub async fn open_session(&self, attach: Attach) -> Result<SessionId, CoreError> {
        open::open_session(self, attach).await
    }

    pub fn providers(&self) -> Arc<ProviderRegistry> {
        self.providers
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn config(&self) -> Arc<Config> {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|c| c.into_inner().clone())
    }

    /// Re-read config (api keys, providers, defaults) for NEW sessions. Existing
    /// sessions keep their registry. Called by the daemon on every `Attach`.
    pub fn reload_config(
        &self,
        config: Config,
        api_keys: &std::collections::BTreeMap<String, Option<String>>,
    ) -> Result<(), CoreError> {
        let mut registry = ProviderRegistry::from_config(&config, api_keys)?;
        // keep a mock provider inserted at startup (tests/smoke)
        if let Some(mock) = self.providers().get("mock") {
            registry.insert(mock);
            if registry.default_route().is_none() {
                registry
                    .set_default_route(Some(pacode_types::ModelRoute::new("mock", "mock-model")));
            }
        }
        if let Ok(mut p) = self.providers.write() {
            *p = Arc::new(registry);
        }
        if let Ok(mut c) = self.config.write() {
            *c = Arc::new(config);
        }
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.sessions.read().ok()?.get(id).cloned()
    }

    pub fn session_ids(&self) -> Vec<SessionId> {
        self.sessions
            .read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Event stream of a session: `(seq, event)`, seq monotonic per session.
    pub fn subscribe(&self, id: &SessionId) -> Option<broadcast::Receiver<(u64, Event)>> {
        self.session(id).map(|s| s.events.subscribe())
    }

    /// Handle every request except the connection-level ones (`Hello`, `Attach`,
    /// `Detach`, `Shutdown`, `Ping`), which the daemon answers itself. Unknown or
    /// inapplicable requests return `Reply::Error`.
    pub async fn handle(&self, id: &SessionId, req: Request) -> Reply {
        if let Some(reply) = self.handle_global(&req).await {
            return reply;
        }
        let session = match self.session(id) {
            Some(s) => s,
            None => {
                return Reply::Error {
                    message: "session not found".to_string(),
                };
            }
        };

        match req {
            Request::UserMessage { text } => match session.submit_user_message(text).await {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::Interrupt => {
                session.interrupt();
                Reply::Ok
            }
            Request::PermissionReply {
                permission,
                decision,
            } => {
                if session.resolve_permission(&permission, decision) {
                    Reply::Ok
                } else {
                    Reply::Error {
                        message: "unknown permission request".to_string(),
                    }
                }
            }
            Request::SetModel(route) => match session.set_model(route).await {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::SetEffort(effort) => {
                session.set_effort(effort).await;
                Reply::Ok
            }
            Request::SetMode(mode) => {
                session.set_mode(mode);
                Reply::Ok
            }
            Request::StopAgent(agent_id) => match session.stop_agent(&agent_id).await {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::KillTask(task_id) => match self.deps.tasks.kill(&task_id).await {
                Ok(()) => {
                    if let Some(info) = session.tasks.info(&task_id) {
                        session.events.emit(Event::TaskUpdated(info));
                    }
                    Reply::Ok
                }
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::AckTask(task_id) => {
                self.deps.tasks.ack(&task_id);
                if let Some(info) = session.tasks.info(&task_id) {
                    session.events.emit(Event::TaskUpdated(info));
                }
                Reply::Ok
            }
            Request::GetSnapshot => Reply::Snapshot(session.snapshot()),
            Request::GetHistory {
                agent,
                before_seq,
                limit,
            } => {
                let agent_obj = match session.agent(&agent) {
                    Some(a) => a,
                    None => {
                        return Reply::Error {
                            message: "agent not found".to_string(),
                        };
                    }
                };
                let (tail_items, has_more) = agent_obj.transcript_tail(before_seq, limit as usize);
                if tail_items.len() >= limit as usize || !has_more {
                    Reply::History {
                        agent,
                        items: tail_items,
                        has_more,
                    }
                } else {
                    let remaining_limit = (limit as usize).saturating_sub(tail_items.len()) as u32;
                    let earliest_seq = tail_items.first().map(|i| i.seq).or(before_seq);
                    let older_messages = session
                        .store
                        .load_messages_before(&session.id, &agent, earliest_seq, remaining_limit)
                        .await
                        .unwrap_or_default();
                    let older_tuples: Vec<(u64, Arc<pacode_types::Message>)> = older_messages
                        .into_iter()
                        .map(|row| (row.seq, Arc::new(row.message)))
                        .collect();
                    let first_seq = older_tuples.first().map(|(s, _)| *s).unwrap_or(0);
                    let mut older_items =
                        crate::transcript::history_to_items(&agent, &older_tuples, first_seq);
                    let has_even_more = older_items.len() >= remaining_limit as usize;
                    older_items.extend(tail_items);
                    Reply::History {
                        agent,
                        items: older_items,
                        has_more: has_even_more,
                    }
                }
            }
            Request::GetTaskOutput { task, tail_lines } => {
                match self.deps.tasks.tail(&task, tail_lines as usize).await {
                    Ok(lines) => {
                        let count = lines.len() as u64;
                        Reply::TaskOutput {
                            task,
                            lines,
                            total_lines: count,
                        }
                    }
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::ListSessions { limit } => {
                match self
                    .deps
                    .store
                    .list_sessions(pacode_store::SessionFilter { cwd: None, limit })
                    .await
                {
                    Ok(sessions) => Reply::Sessions { sessions },
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::ListModels => {
                let models = self.providers().list_all_models().await;
                Reply::Models { models }
            }
            Request::AnswerQuestion { question, answer } => {
                match session.questions.resolve(&question, answer.clone()) {
                    Some(q) => {
                        session.record_question_answer(&q, &answer);
                        session
                            .events
                            .emit(Event::QuestionResolved { question, answer });
                        Reply::Ok
                    }
                    None => Reply::Error {
                        message: "unknown question".to_string(),
                    },
                }
            }
            Request::ListCronJobs => Reply::CronJobs {
                jobs: session.scheduler.jobs(),
            },
            Request::AddCronJob {
                name,
                schedule,
                prompt,
            } => match session.scheduler.add_job(
                name,
                schedule,
                prompt,
                pacode_types::time::now_ms(),
            ) {
                Ok(job) => {
                    if let Err(e) = session.store.upsert_cron_job(&session.id, &job).await {
                        log::warn!("failed to persist cron job {}: {e}", job.id);
                    }
                    session.events.emit(Event::CronUpdated(job));
                    Reply::CronJobs {
                        jobs: session.scheduler.jobs(),
                    }
                }
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::RemoveCronJob(id) => match session.scheduler.remove_job(&id) {
                Ok(()) => {
                    if let Err(e) = session.store.delete_cron_job(&id).await {
                        log::warn!("failed to delete cron job {id}: {e}");
                    }
                    session.events.emit(Event::CronRemoved(id));
                    Reply::CronJobs {
                        jobs: session.scheduler.jobs(),
                    }
                }
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::SetCronEnabled { id, enabled } => {
                match session
                    .scheduler
                    .set_enabled(&id, enabled, pacode_types::time::now_ms())
                {
                    Ok(job) => {
                        if let Err(e) = session.store.upsert_cron_job(&session.id, &job).await {
                            log::warn!("failed to persist cron job {}: {e}", job.id);
                        }
                        session.events.emit(Event::CronUpdated(job));
                        Reply::CronJobs {
                            jobs: session.scheduler.jobs(),
                        }
                    }
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::RunCronJobNow(id) => {
                match session.scheduler.run_now(&id, pacode_types::time::now_ms()) {
                    Ok(job) => {
                        session.events.emit(Event::CronUpdated(job));
                        Reply::CronJobs {
                            jobs: session.scheduler.jobs(),
                        }
                    }
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::ListMonitors => Reply::Monitors {
                monitors: session.scheduler.monitors(),
            },
            Request::StopMonitor(id) => match session.scheduler.stop_monitor(&id) {
                Ok(info) => {
                    session.events.emit(Event::MonitorUpdated(info));
                    Reply::Monitors {
                        monitors: session.scheduler.monitors(),
                    }
                }
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::Compact => {
                if let Some(main) = session.main_agent() {
                    match crate::compaction::compact(&session, &main).await {
                        Ok(_) => Reply::Ok,
                        Err(e) => Reply::Error {
                            message: e.to_string(),
                        },
                    }
                } else {
                    Reply::Error {
                        message: "main agent not found".to_string(),
                    }
                }
            }
            Request::DetachTurn => match session.detach_main_turn().await {
                Ok(agent) => Reply::TurnDetached { agent },
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::Hello(_)
            | Request::Attach(_)
            | Request::Detach
            | Request::Shutdown { .. }
            | Request::Ping => Reply::Error {
                message: "connection-level request handled by daemon".to_string(),
            },
            Request::ListMcpServers
            | Request::RestartMcpServer { .. }
            | Request::SetMcpServerEnabled { .. }
            | Request::GetMcpPrompt { .. }
            | Request::ListPlugins
            | Request::RunPluginCommand { .. }
            | Request::BrowseMarketplace { .. }
            | Request::InstallPlugin { .. }
            | Request::UninstallPlugin { .. }
            | Request::ListAuth
            | Request::Login { .. }
            | Request::Logout { .. }
            | Request::SetAuthAccount { .. } => {
                self.handle_global(&req)
                    .await
                    .unwrap_or_else(|| Reply::Error {
                        message: "unhandled request".to_string(),
                    })
            }
        }
    }

    pub fn set_auth_driver(&self, driver: Arc<dyn crate::auth::AuthDriver>) {
        if let Ok(mut g) = self.auth_driver.write() {
            *g = driver;
        }
    }

    pub fn load_auth_store(&self) -> pacode_auth::store::AuthStore {
        let path = self.deps.paths.auth_file();
        pacode_auth::store::AuthStore::load_from(&path)
            .unwrap_or_else(|_| pacode_auth::store::AuthStore::new_empty(path))
    }

    pub fn handle_list_auth(&self) -> Reply {
        let store = self.load_auth_store();
        let providers = crate::auth::build_auth_status(&store, &self.config().providers);
        Reply::AuthStatus { providers }
    }

    pub fn handle_login(&self, provider: String) -> Reply {
        let driver = self
            .auth_driver
            .read()
            .map(|d| Arc::clone(&*d))
            .unwrap_or_else(|p| Arc::clone(&*p.into_inner()));
        let core_weak = self.self_weak.get().cloned().unwrap_or_default();
        let provider_clone = provider.clone();

        tokio::spawn(async move {
            let on_progress_provider = provider_clone.clone();
            let on_progress_weak = core_weak.clone();
            let on_progress: crate::auth::ProgressCallback = Arc::new(move |progress| {
                if let Some(core) = on_progress_weak.upgrade() {
                    let stage = match progress {
                        crate::auth::Progress::OpenUrl { url, opened } => {
                            pacode_types::protocol::LoginStage::OpenUrl { url, opened }
                        }
                        crate::auth::Progress::Waiting => {
                            pacode_types::protocol::LoginStage::Waiting
                        }
                        crate::auth::Progress::Exchanging => {
                            pacode_types::protocol::LoginStage::Exchanging
                        }
                    };
                    core.broadcast_event(Event::LoginProgress {
                        provider: on_progress_provider.clone(),
                        stage,
                    });
                }
            });

            match driver.login(&provider_clone, on_progress).await {
                Ok(outcome) => {
                    if let Some(core) = core_weak.upgrade() {
                        let mut store = core.load_auth_store();
                        store.upsert(&provider_clone, outcome.account.clone());
                        if let Err(e) = store.save() {
                            log::warn!("failed to save auth store after login: {e}");
                        }

                        let _ = core
                            .providers()
                            .rebuild_provider_from_store(&provider_clone, &store);

                        if let Some(info) = crate::auth::get_provider_auth_info(
                            &store,
                            &core.config().providers,
                            &provider_clone,
                        ) {
                            core.broadcast_event(Event::AuthUpdated(info));
                        }

                        core.broadcast_event(Event::LoginProgress {
                            provider: provider_clone,
                            stage: pacode_types::protocol::LoginStage::Done {
                                label: outcome.account.label,
                            },
                        });
                    }
                }
                Err(err) => {
                    if let Some(core) = core_weak.upgrade() {
                        core.broadcast_event(Event::LoginProgress {
                            provider: provider_clone,
                            stage: pacode_types::protocol::LoginStage::Failed { message: err },
                        });
                    }
                }
            }
        });

        Reply::Ok
    }

    pub fn handle_logout(&self, provider: String, label: Option<String>) -> Reply {
        let mut store = self.load_auth_store();
        if let Some(lbl) = &label {
            store.remove(&provider, lbl);
        } else {
            let accounts = store.accounts(&provider).to_vec();
            for acc in accounts {
                store.remove(&provider, &acc.label);
            }
        }
        if let Err(e) = store.save() {
            log::warn!("failed to save auth store after logout: {e}");
        }

        let _ = self
            .providers()
            .rebuild_provider_from_store(&provider, &store);

        if let Some(info) =
            crate::auth::get_provider_auth_info(&store, &self.config().providers, &provider)
        {
            self.broadcast_event(Event::AuthUpdated(info));
        }

        let providers = crate::auth::build_auth_status(&store, &self.config().providers);
        Reply::AuthStatus { providers }
    }

    pub fn handle_set_auth_account(&self, provider: String, label: String) -> Reply {
        let mut store = self.load_auth_store();
        if let Err(e) = store.set_active(&provider, &label) {
            return Reply::Error {
                message: e.to_string(),
            };
        }
        if let Err(e) = store.save() {
            log::warn!("failed to save auth store after set_auth_account: {e}");
        }

        let _ = self
            .providers()
            .rebuild_provider_from_store(&provider, &store);

        if let Some(info) =
            crate::auth::get_provider_auth_info(&store, &self.config().providers, &provider)
        {
            self.broadcast_event(Event::AuthUpdated(info));
        }

        let providers = crate::auth::build_auth_status(&store, &self.config().providers);
        Reply::AuthStatus { providers }
    }

    pub fn broadcast_event(&self, event: Event) {
        if let Ok(guard) = self.sessions.read() {
            for session in guard.values() {
                session.events.emit(event.clone());
            }
        }
    }

    pub fn snapshot(&self, id: &SessionId) -> Option<SessionSnapshot> {
        self.session(id).map(|s| s.snapshot())
    }

    /// No running turns, no live subagents, no running tasks in any session.
    pub fn is_idle(&self) -> bool {
        let sessions = match self.sessions.read() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        for session in sessions.values() {
            if session.is_busy() {
                return false;
            }
        }
        true
    }

    /// Stop agents, kill the session's tasks, persist, drop from memory.
    pub async fn close_session(&self, id: &SessionId) {
        let session = {
            let mut sessions = match self.sessions.write() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            sessions.remove(id)
        };
        if let Some(session) = session {
            crate::hooks::session_end(&session);
            let agents = session
                .agents
                .read()
                .map(|a| a.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for agent in agents {
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
                let _ = session
                    .store
                    .upsert_agent(&session.id, &info, agent.prompt().as_deref())
                    .await;
            }
            session.tasks.kill_session(&session.id).await;
        }
    }

    /// Close every session, stop the router, shut down tasks and MCP servers, close
    /// the store.
    pub async fn shutdown(&self) {
        if let Some(handle) = self
            .task_router
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            handle.abort();
        }
        let ids = self.session_ids();
        for id in ids {
            self.close_session(&id).await;
        }
        self.deps.tasks.shutdown().await;
        self.deps.mcp.shutdown().await;
        self.deps.store.clone().close().await;
    }
}
