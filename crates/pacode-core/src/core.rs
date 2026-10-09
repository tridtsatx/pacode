//! `Core`: the daemon-facing API.

pub(crate) mod global;
pub(crate) mod open;
pub(crate) mod router;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_store::Store;
use pacode_tools::{Tool, ToolRegistry};
use pacode_types::{
    Attach, Config, Event, ImageSource, Reply, Request, SessionId, SessionSnapshot, TaskId,
};
use tokio::sync::broadcast;

use crate::CoreError;
use crate::session::Session;

type UserMessageIdSet = HashSet<(String, u64)>;
type SessionUserMessageIds = Arc<tokio::sync::Mutex<UserMessageIdSet>>;

pub(crate) fn user_message_fingerprint(text: &str, images: &[ImageSource]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    images.len().hash(&mut hasher);
    for image in images {
        match image {
            ImageSource::Path(path) => {
                0u8.hash(&mut hasher);
                path.hash(&mut hasher);
            }
            ImageSource::Bytes { mime, data } => {
                1u8.hash(&mut hasher);
                mime.hash(&mut hasher);
                data.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

const ADMISSION_WARM_SESSIONS: u32 = 8;
const ADMISSION_WARM_PER_SESSION: usize = 1024;

async fn warm_message_admissions(store: &Store) -> BTreeMap<SessionId, SessionUserMessageIds> {
    let filter = pacode_store::SessionFilter {
        cwd: None,
        limit: ADMISSION_WARM_SESSIONS,
    };
    let sessions = match store.list_sessions(filter).await {
        Ok(sessions) => sessions,
        Err(error) => {
            log::warn!("failed to list recent sessions for the message admission cache: {error}");
            return BTreeMap::new();
        }
    };
    let mut warmed: BTreeMap<SessionId, SessionUserMessageIds> = BTreeMap::new();
    for meta in sessions {
        match store.load_message_admissions_for_session(&meta.id).await {
            Ok(admissions) => {
                let ids: UserMessageIdSet = admissions
                    .into_iter()
                    .rev()
                    .take(ADMISSION_WARM_PER_SESSION)
                    .map(|admission| (admission.message_id, admission.fingerprint))
                    .collect();
                warmed.insert(meta.id, Arc::new(tokio::sync::Mutex::new(ids)));
            }
            Err(error) => {
                log::warn!(
                    "failed to warm the message admission cache for {}: {error}",
                    meta.id
                );
            }
        }
    }
    warmed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeaseKind {
    Attach,
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLease {
    session: SessionId,
    id: u64,
    kind: LeaseKind,
}

impl SessionLease {
    pub fn session(&self) -> &SessionId {
        &self.session
    }
}

#[derive(Default)]
pub(crate) struct SessionLeases {
    next_id: AtomicU64,
    held: RwLock<BTreeMap<SessionId, BTreeSet<(LeaseKind, u64)>>>,
    detached: RwLock<BTreeSet<SessionId>>,
}

impl SessionLeases {
    fn acquire(&self, session: &SessionId, kind: LeaseKind) -> SessionLease {
        let id = self.next_id.fetch_add(1, AtomicOrdering::SeqCst);
        self.held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(session.clone())
            .or_default()
            .insert((kind, id));
        if kind == LeaseKind::Attach {
            self.detached
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(session);
        }
        SessionLease {
            session: session.clone(),
            id,
            kind,
        }
    }

    fn release(&self, lease: &SessionLease) -> bool {
        let mut held = self
            .held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(leases) = held.get_mut(&lease.session) else {
            return false;
        };
        leases.remove(&(lease.kind, lease.id));
        let mut detached = self
            .detached
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if lease.kind == LeaseKind::Attach {
            detached.insert(lease.session.clone());
        }
        if !leases.is_empty() {
            return false;
        }
        held.remove(&lease.session);
        detached.contains(&lease.session)
    }

    fn holds(&self, session: &SessionId) -> bool {
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session)
            .is_some_and(|leases| !leases.is_empty())
    }

    fn is_candidate(&self, session: &SessionId) -> bool {
        !self.holds(session)
            && self
                .detached
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(session)
    }

    fn count(&self, session: &SessionId, kind: LeaseKind) -> usize {
        self.held
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session)
            .map(|leases| leases.iter().filter(|(held, _)| *held == kind).count())
            .unwrap_or(0)
    }

    fn forget(&self, session: &SessionId) {
        self.held
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session);
        self.detached
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session);
    }
}

struct RequestLease<'a> {
    core: &'a Core,
    lease: SessionLease,
}

impl Drop for RequestLease<'_> {
    fn drop(&mut self) {
        self.core.release_lease(&self.lease);
    }
}

#[derive(Clone, Default)]
pub struct IdleWatch(Option<std::sync::Weak<Core>>);

impl IdleWatch {
    pub(crate) fn attach(core: &Core) -> Self {
        Self(core.self_weak.get().cloned())
    }

    pub(crate) fn notify(&self, session: &Session) {
        if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(core) = self.0.as_ref().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        if !core.is_eviction_candidate(&session.id) {
            return;
        }
        core.spawn_idle_eviction(&session.id);
    }
}

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
    session_locks: RwLock<BTreeMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) leases: SessionLeases,
    pub(crate) closing: AtomicBool,
    pub(crate) idempotent_sessions: RwLock<BTreeMap<SessionId, SessionId>>,
    pub(crate) user_message_ids: tokio::sync::RwLock<BTreeMap<SessionId, SessionUserMessageIds>>,
    pub(crate) self_weak: std::sync::OnceLock<std::sync::Weak<Core>>,
    /// Routes `TaskEvent`s to the owning session (injections + UI events).
    pub(crate) task_router: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) task_tombstones: std::sync::Mutex<HashSet<TaskId>>,
    pub(crate) cached_mcp_tools: std::sync::Mutex<Option<Vec<Arc<dyn Tool>>>>,
    /// Events that belong to the daemon rather than to a session (login progress,
    /// credential changes). A client sees them without attaching to a session.
    pub(crate) global_events: crate::transcript::EventSink,
    /// How long session opens take, and how many history pages were served.
    /// Atomics updated on the path that does the work — nothing polls them.
    resume_hist: pacode_types::LatencyHist,
    history_pages: AtomicU64,
    /// One held lease per live session, keeping that session's share of the tool
    /// memory ceiling. The lease is the ownership: dropping it ends the
    /// division, so there is no "close" call to forget on an error path.
    memory_leases: RwLock<BTreeMap<SessionId, pacode_exec::memlimit::SessionLease>>,
}

impl Core {
    /// Build the core and start the task-event router.
    pub async fn new(deps: CoreDeps) -> Arc<Core> {
        let rx = deps.tasks.subscribe();
        let providers = RwLock::new(deps.providers.clone());
        let config = RwLock::new(deps.config.clone());
        let auth_driver: RwLock<Arc<dyn crate::auth::AuthDriver>> =
            RwLock::new(Arc::new(crate::auth::DefaultAuthDriver));
        let user_message_ids = warm_message_admissions(&deps.store).await;
        let core = Arc::new(Core {
            deps,
            providers,
            config,
            auth_driver,
            sessions: RwLock::new(BTreeMap::new()),
            session_locks: RwLock::new(BTreeMap::new()),
            leases: SessionLeases::default(),
            closing: AtomicBool::new(false),
            idempotent_sessions: RwLock::new(BTreeMap::new()),
            user_message_ids: tokio::sync::RwLock::new(user_message_ids),
            self_weak: std::sync::OnceLock::new(),
            task_router: std::sync::Mutex::new(None),
            task_tombstones: std::sync::Mutex::new(HashSet::new()),
            cached_mcp_tools: std::sync::Mutex::new(None),
            global_events: crate::transcript::EventSink::new(),
            resume_hist: pacode_types::LatencyHist::new(),
            history_pages: AtomicU64::new(0),
            memory_leases: RwLock::new(BTreeMap::new()),
        });

        let core_weak = Arc::downgrade(&core);
        let _ = core.self_weak.set(core_weak.clone());
        let handle = router::start_task_router(core_weak, rx);
        *core.task_router.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
        core
    }

    /// MCP tool proxies built purely from the schema cache — no server is
    /// spawned here: a hit contributes its tools, a miss contributes nothing.
    /// `open_session` uses this so attach never waits on MCP work; the
    /// detached [`Self::spawn_mcp_tools_refresh`] task then runs the real
    /// listing and swaps the merged registry into every open session.
    /// A set the refresh (or `SetMcpServerEnabled`) already produced is
    /// itself a cache and is served as-is.
    pub(crate) fn get_mcp_tools_cached(&self) -> Vec<Arc<dyn Tool>> {
        if self.deps.mcp.server_names().is_empty() {
            return Vec::new();
        }
        if let Ok(guard) = self.cached_mcp_tools.lock()
            && let Some(tools) = guard.as_ref()
        {
            return tools.clone();
        }
        let tools = pacode_tools::builtin::mcp::mcp_tools_cached(self.deps.mcp.clone());
        if let Ok(mut guard) = self.cached_mcp_tools.lock() {
            *guard = Some(tools.clone());
        }
        tools
    }

    /// Post-attach MCP discovery, off the attach path: the real listing —
    /// which starts `lazy: false` servers and reaps idle ones — runs in a
    /// detached task, then the merged registry replaces every session's (and
    /// its main agent's) tools, the same swap `SetMcpServerEnabled` performs.
    /// The schema cache wrote the misses down, so the next open sees them.
    pub(crate) fn spawn_mcp_tools_refresh(&self) {
        if self.deps.mcp.server_names().is_empty() {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            let Some(core) = weak.upgrade() else {
                return;
            };
            let fresh_mcp_tools =
                pacode_tools::builtin::mcp::mcp_tools(core.deps.mcp.clone()).await;
            if let Ok(mut guard) = core.cached_mcp_tools.lock() {
                *guard = Some(fresh_mcp_tools.clone());
            }
            let mut new_tools = core.deps.tools.clone();
            for tool in &fresh_mcp_tools {
                new_tools.register(tool.clone());
            }
            if let Ok(sessions) = core.sessions.read() {
                for session in sessions.values() {
                    if let Ok(mut g) = session.tools.write() {
                        *g = new_tools.clone();
                    }
                    if let Some(main) = session.main_agent()
                        && let Ok(mut g) = main.tools.write()
                    {
                        *g = new_tools.clone();
                    }
                }
            }
        });
    }

    /// `Attach::New` creates a session (persisted immediately); `Resume` loads meta,
    /// plan, agents (as finished), tasks (as finished/killed) and the main agent's
    /// history (compaction summary + messages after it) from the store; `Latest`
    /// resumes the most recently updated session for `cwd` or creates one.
    pub async fn open_session(&self, attach: Attach) -> Result<SessionId, CoreError> {
        let id = open::open_session(self, attach).await?;
        // Held for as long as the session is open, and given back by
        // `close_session`. Re-opening replaces the lease, so a reconnecting
        // client does not claim two shares of the ceiling.
        let lease = self.deps.tasks.memory().session_opened(&id);
        self.memory_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), lease);
        Ok(id)
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
    /// sessions keep the provider instances they already resolved. Called by the
    /// daemon on every `Attach`.
    ///
    /// The registry is reloaded **in place** rather than replaced. Replacing the
    /// `Arc` would swap the provider out from under a running turn and would drop
    /// the shared [`pacode_provider::CatalogCache`] along with it, refetching every
    /// catalog; `ProviderRegistry::reload` rebuilds only the entries whose config
    /// changed and keeps the cache. Per the registry's own contract, a turn that
    /// already holds a provider keeps that instance until it finishes and only
    /// new lookups see the new world.
    pub fn reload_config(
        &self,
        config: Config,
        api_keys: &std::collections::BTreeMap<String, Option<String>>,
    ) -> Result<(), CoreError> {
        let auth = self.load_auth_store();
        let report = self
            .providers()
            .reload(&config, api_keys, Some(&auth))
            .map_err(CoreError::Provider)?;
        log::debug!(
            "config reloaded: {} added, {} rebuilt, {} removed, {} unchanged",
            report.added.len(),
            report.rebuilt.len(),
            report.removed.len(),
            report.unchanged.len()
        );
        if let Ok(mut c) = self.config.write() {
            *c = Arc::new(config);
        }
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.sessions.read().ok()?.get(id).cloned()
    }

    pub async fn attach_lease(&self, id: &SessionId) -> Result<SessionLease, CoreError> {
        let lifecycle_lock = self.session_lock(id);
        let lifecycle_guard = lifecycle_lock.lock().await;
        if self.session(id).is_none() {
            return Err(CoreError::SessionNotFound(id.clone()));
        }
        let lease = self.leases.acquire(id, LeaseKind::Attach);
        drop(lifecycle_guard);
        self.spawn_idle_sweep();
        Ok(lease)
    }

    pub fn is_leased(&self, id: &SessionId) -> bool {
        self.leases.holds(id)
    }

    pub fn is_eviction_candidate(&self, id: &SessionId) -> bool {
        self.leases.is_candidate(id)
    }

    pub fn lease_count(&self, id: &SessionId, kind: LeaseKind) -> usize {
        self.leases.count(id, kind)
    }

    pub fn release_lease(&self, lease: &SessionLease) {
        if self.leases.release(lease) {
            self.spawn_idle_eviction(lease.session());
        }
    }

    fn request_lease(&self, id: &SessionId) -> RequestLease<'_> {
        RequestLease {
            core: self,
            lease: self.leases.acquire(id, LeaseKind::Request),
        }
    }

    pub(crate) fn spawn_idle_eviction(&self, id: &SessionId) {
        if self.closing.load(AtomicOrdering::SeqCst) {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let id = id.clone();
        handle.spawn(async move {
            let Some(core) = weak.upgrade() else {
                return;
            };
            core.evict_if_idle(&id).await;
        });
    }

    fn spawn_idle_sweep(&self) {
        if self.closing.load(AtomicOrdering::SeqCst) {
            return;
        }
        let candidates: Vec<SessionId> = {
            let Ok(sessions) = self.sessions.read() else {
                return;
            };
            sessions
                .keys()
                .filter(|id| self.leases.is_candidate(id))
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let Some(core) = weak.upgrade() else {
                return;
            };
            for id in candidates {
                core.evict_if_idle(&id).await;
            }
        });
    }

    pub async fn evict_if_idle(&self, id: &SessionId) -> bool {
        if self.closing.load(AtomicOrdering::SeqCst) {
            return false;
        }
        let lifecycle_lock = self.session_lock(id);
        let lifecycle_guard = lifecycle_lock.lock().await;
        if !self.leases.is_candidate(id) {
            return false;
        }
        let Some(session) = self.session(id) else {
            self.leases.forget(id);
            return false;
        };
        if session.has_live_work() {
            return false;
        }
        let unloaded = {
            let mut sessions = self
                .sessions
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match sessions.get(id) {
                Some(current) if Arc::ptr_eq(current, &session) => {
                    sessions.remove(id);
                    true
                }
                _ => false,
            }
        };
        if !unloaded {
            return false;
        }
        self.leases.forget(id);
        self.user_message_ids.write().await.remove(id);
        drop(session);
        drop(lifecycle_guard);
        log::info!("evicted idle session {id}");
        true
    }

    pub(crate) fn session_lock(&self, id: &SessionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .session_locks
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            locks
                .entry(id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
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

    async fn submit_client_user_message(
        &self,
        session: &Arc<Session>,
        text: String,
        images: Vec<ImageSource>,
        message_id: Option<String>,
    ) -> Reply {
        let admission =
            message_id.map(|message_id| (message_id, user_message_fingerprint(&text, &images)));
        let result = match admission.as_ref() {
            None => session.submit_user_message_with_images(text, images).await,
            Some((message_id, fingerprint)) => {
                session
                    .submit_user_message_with_images_idempotent(
                        text,
                        images,
                        message_id.clone(),
                        *fingerprint,
                    )
                    .await
            }
        };

        if let Some((message_id, fingerprint)) = admission
            && result.is_ok()
        {
            let ids = self
                .user_message_ids
                .write()
                .await
                .entry(session.id.clone())
                .or_default()
                .clone();
            ids.lock().await.insert((message_id, fingerprint));
        }

        match result {
            Ok(_) => Reply::Ok,
            Err(error) => Reply::Error {
                message: error.to_string(),
            },
        }
    }

    pub(crate) fn is_task_tombstoned(&self, task: &TaskId) -> bool {
        self.task_tombstones
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(task)
    }

    pub(crate) fn tombstone_tasks(&self, tasks: impl IntoIterator<Item = TaskId>) {
        self.task_tombstones
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(tasks);
    }

    fn task_for_session(&self, session: &Session, task: &TaskId) -> Option<pacode_types::TaskInfo> {
        self.deps
            .tasks
            .info(task)
            .filter(|info| info.session == session.id)
    }

    /// Handle every request except the connection-level ones (`Hello`, `Attach`,
    /// `Detach`, `Shutdown`, `Ping`), which the daemon answers itself. Unknown or
    /// inapplicable requests return `Reply::Error`.
    pub async fn handle(&self, id: &SessionId, req: Request) -> Reply {
        if let Some(reply) = self.handle_global(&req).await {
            return reply;
        }
        let _request_lease = self.request_lease(id);
        let session = match self.session(id) {
            Some(s) => s,
            None => {
                return Reply::Error {
                    message: "session not found".to_string(),
                };
            }
        };

        match req {
            // Handled above, before a session is resolved: the caller does not
            // have to be attached to ask.
            Request::RuntimeStats => unreachable!("runtime stats are answered globally"),
            Request::Analyze => Reply::Analyze {
                // A point-in-time copy: the page outlives the borrow, and a
                // client reading an older report is reading history, not a
                // half-updated ledger.
                report: Box::new(match self.session(id) {
                    Some(session) => session.accounting.report(),
                    None => pacode_types::analyze::AnalyzeReport::default(),
                }),
            },
            Request::CloseSession => {
                self.close_session(id).await;
                Reply::Ok
            }
            Request::UserMessage {
                text,
                images,
                message_id,
            } => {
                self.submit_client_user_message(&session, text, images, message_id)
                    .await
            }
            Request::Interrupt => {
                session.interrupt();
                Reply::Ok
            }
            Request::InterruptAll => {
                // Main agent's turn plus every live subagent.
                session.interrupt_all().await;
                Reply::Ok
            }
            Request::PermissionReply {
                permission,
                decision,
                reason,
            } => {
                if session.resolve_permission(&permission, decision, reason) {
                    Reply::Ok
                } else {
                    Reply::Error {
                        message: "unknown permission request".to_string(),
                    }
                }
            }
            Request::ExplainPermission { permission } => {
                match session.permissions.get(&permission) {
                    Some(req) => {
                        // Accepted: the model call runs detached — this handler
                        // must not wait on an LLM round-trip. The result is
                        // broadcast as `Event::PermissionExplained`.
                        crate::explain::spawn(session.clone(), req);
                        Reply::Ok
                    }
                    None => Reply::Error {
                        message: "unknown permission request".to_string(),
                    },
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
            Request::SetZdr(zdr) => match session.set_zdr(zdr) {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::SetNoticeLock(locked) => {
                session.set_notice_lock(locked).await;
                Reply::Ok
            }
            Request::SetModelFlag { model, flags } => match session.set_model_flags(model, flags) {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::StopAgent(agent_id) => match session.stop_agent(&agent_id).await {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
            Request::KillTask(task_id) => {
                let Some(info) = self.task_for_session(&session, &task_id) else {
                    return Reply::Error {
                        message: "task not found".to_string(),
                    };
                };
                match self.deps.tasks.kill(&task_id).await {
                    Ok(()) => {
                        if let Some(info) = session.tasks.info(&task_id) {
                            session.events.emit(Event::TaskUpdated(info));
                        } else {
                            session.events.emit(Event::TaskUpdated(info));
                        }
                        Reply::Ok
                    }
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::AckTask(task_id) => {
                let Some(info) = self.task_for_session(&session, &task_id) else {
                    return Reply::Error {
                        message: "task not found".to_string(),
                    };
                };
                self.deps.tasks.ack(&task_id);
                if let Some(info) = session.tasks.info(&task_id) {
                    session.events.emit(Event::TaskUpdated(info));
                } else {
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
                let limit = limit.clamp(1, 500);
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
                    // The tail cannot fill the page, so the rest comes from the
                    // store. That read is positioned by the agent's own
                    // message-seq boundary, never by the caller's item cursor:
                    // the two numberings are unrelated, and deriving one from
                    // the other read the wrong window.
                    let room = (limit as usize).saturating_sub(tail_items.len());
                    self.record_history_page();
                    let (mut older_items, older_has_more) = agent_obj
                        .load_older_history(&session.store, &session.id, room)
                        .await;
                    older_items.extend(tail_items);
                    Reply::History {
                        agent,
                        items: older_items,
                        has_more: older_has_more,
                    }
                }
            }
            Request::GetTaskOutput { task, tail_lines } => {
                if self.task_for_session(&session, &task).is_none() {
                    return Reply::Error {
                        message: "task not found".to_string(),
                    };
                }
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
                        session.emit_session_status();
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
            Request::GitLog { after } => {
                let cwd = session.meta().cwd;
                match pacode_vcs::log(&cwd, after.as_deref(), pacode_vcs::LOG_PAGE_SIZE).await {
                    Ok(page) => Reply::GitLog {
                        commits: page.commits,
                        has_more: page.has_more,
                        status: pacode_types::GitStatus::Ready,
                    },
                    Err(pacode_vcs::VcsError::NotARepo) => Reply::GitLog {
                        commits: Vec::new(),
                        has_more: false,
                        status: pacode_types::GitStatus::NotARepo,
                    },
                    Err(pacode_vcs::VcsError::GitMissing) => Reply::GitLog {
                        commits: Vec::new(),
                        has_more: false,
                        status: pacode_types::GitStatus::GitMissing,
                    },
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::GitShow { hash } => {
                let cwd = session.meta().cwd;
                match pacode_vcs::show_stat(&cwd, &hash).await {
                    Ok(stat) => Reply::GitShow { hash, stat },
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::GitDiff { hash } => {
                let cwd = session.meta().cwd;
                match pacode_vcs::commit_diff(&cwd, &hash).await {
                    Ok((patch, truncated)) => Reply::GitDiff {
                        hash,
                        patch,
                        truncated,
                    },
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::Compact => {
                if let Some(main) = session.main_agent() {
                    if main.is_running() {
                        return Reply::Error {
                            message: "cannot compact while a turn is running".to_string(),
                        };
                    }
                    let _compact_guard = session.submit_lock.lock().await;
                    if main.is_running() {
                        return Reply::Error {
                            message: "cannot compact while a turn is running".to_string(),
                        };
                    }
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
            Request::ClearContext => match crate::clear::clear_context(&session).await {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error {
                    message: e.to_string(),
                },
            },
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
            | Request::SetAuthAccount { .. }
            | Request::RefreshModels => {
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
                    core.emit_global(Event::LoginProgress {
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
                        core.announce_catalog(&provider_clone);

                        if let Some(info) = crate::auth::get_provider_auth_info(
                            &store,
                            &core.config().providers,
                            &provider_clone,
                        ) {
                            core.emit_global(Event::AuthUpdated(info));
                        }

                        core.emit_global(Event::LoginProgress {
                            provider: provider_clone,
                            stage: pacode_types::protocol::LoginStage::Done {
                                label: outcome.account.label,
                            },
                        });
                    }
                }
                Err(err) => {
                    if let Some(core) = core_weak.upgrade() {
                        core.emit_global(Event::LoginProgress {
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
        self.announce_catalog(&provider);

        if let Some(info) =
            crate::auth::get_provider_auth_info(&store, &self.config().providers, &provider)
        {
            self.emit_global(Event::AuthUpdated(info));
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
        self.announce_catalog(&provider);

        if let Some(info) =
            crate::auth::get_provider_auth_info(&store, &self.config().providers, &provider)
        {
            self.emit_global(Event::AuthUpdated(info));
        }

        let providers = crate::auth::build_auth_status(&store, &self.config().providers);
        Reply::AuthStatus { providers }
    }

    /// A login, logout or account switch rebuilt a provider, so the catalog
    /// changed: push it to every attached client, or an open model palette
    /// keeps showing the provider as it was until someone asks again. The
    /// rebuild swaps the credential, not the cached catalog, so that
    /// provider's catalog is fetched again first — a new account can see
    /// other models than the old one.
    pub(crate) fn announce_catalog(&self, provider: &str) {
        let core = self.self_weak.get().cloned().unwrap_or_default();
        let provider = provider.to_string();
        tokio::spawn(async move {
            let Some(core) = core.upgrade() else {
                return;
            };
            let providers = core.providers();
            if let Some(handle) = providers.get(&provider) {
                providers
                    .catalog_cache()
                    .force_refresh(&provider, &handle)
                    .await;
            }
            let models = core.providers().list_all_models().await;
            core.broadcast_event(Event::ModelsUpdated { models });
        });
    }

    /// Emit a daemon-wide event. Unlike [`Self::broadcast_event`] this reaches every
    /// connected client, including one that never attached to a session.
    pub fn emit_global(&self, event: Event) -> u64 {
        self.global_events.emit(event)
    }

    /// Subscribe to daemon-wide events.
    pub fn subscribe_global(&self) -> tokio::sync::broadcast::Receiver<(u64, Event)> {
        self.global_events.subscribe()
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

    /// What the process is holding right now, and how long session setup has
    /// been taking. This is the number behind "the session is big now" — resume
    /// is the one place where a long conversation is turned back into resident
    /// state, so its cost has to be visible to be trusted.
    pub fn runtime_stats(&self) -> pacode_types::RuntimeStats {
        let sessions = match self.sessions.read() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };
        let mut stats = pacode_types::RuntimeStats {
            sessions_resident: sessions.len(),
            ..pacode_types::RuntimeStats::default()
        };
        for session in sessions.values() {
            let agents = match session.agents.read() {
                Ok(a) => a,
                Err(p) => p.into_inner(),
            };
            stats.agents_resident += agents.len();
            for agent in agents.values() {
                if let Ok(transcript) = agent.transcript.lock() {
                    stats.transcript_bytes += transcript.tail_bytes();
                    stats.transcript_items += transcript.tail.len() as u64;
                }
                if agent.is_running() {
                    stats.agents_running += 1;
                }
            }
        }
        stats.resume = self.resume_hist.snapshot();
        stats.launches = pacode_types::agent_launch_stats();
        stats.history_pages = self.history_pages.load(AtomicOrdering::Relaxed);
        stats.store = self.deps.store.stats();
        stats
    }

    /// Core and provider instrumentation together: one round trip for
    /// everything a client can ask about how the process is doing.
    pub fn stats(&self) -> (pacode_types::RuntimeStats, pacode_types::ProviderStats) {
        (self.runtime_stats(), pacode_provider::provider_stats())
    }

    /// Record how long a session open took. Called by the open path, which is
    /// the only place that knows the boundary.
    pub(crate) fn record_resume(&self, elapsed: std::time::Duration) {
        self.resume_hist.record_duration(elapsed);
    }

    pub(crate) fn record_history_page(&self) {
        self.history_pages.fetch_add(1, AtomicOrdering::Relaxed);
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
        // The share goes back first, before anything that can return early: a
        // division held by a session that is closing is the exact leak the
        // lease exists to make impossible, and the "already gone" return below
        // must not skip it.
        if let Some(lease) = self
            .memory_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id)
        {
            drop(lease);
        }
        let lifecycle_lock = self.session_lock(id);
        let _lifecycle_guard = lifecycle_lock.lock().await;
        let Some(session) = self.session(id) else {
            return;
        };
        session
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        crate::hooks::session_end(&session);
        if let Some(main) = session.main_agent() {
            main.request_stop();
        }
        let agents = session
            .agents
            .read()
            .map(|a| a.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for agent in &agents {
            agent.request_stop();
            agent.discard_pending_user_messages();
            agent.discard_injections();
            // The token is the signal a turn in flight is waiting on, so it goes
            // out before the drain below: a provider that never streams back only
            // leaves this loop through the cancellation.
            if let Some(cancel) = agent
                .cancel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            {
                cancel.cancel();
            }
        }
        let turn_guards = crate::session::lock_turns(&agents).await;
        let _submit_guard = session.submit_lock.lock().await;
        for agent in &agents {
            let agent_id = agent.id();
            session.cancel_questions_for(&agent_id);
            session.cancel_permissions_for(&agent_id);
            agent.set_status(pacode_types::AgentStatus::Stopped, None);
            let info = agent.info();
            let _ = session
                .store
                .upsert_agent(&session.id, &info, agent.prompt().as_deref())
                .await;
        }
        let _turn_guards = turn_guards;
        let tasks_before_close = session.tasks.list(Some(&session.id));
        self.tombstone_tasks(tasks_before_close.iter().map(|task| task.id.clone()));
        session.tasks.kill_session(&session.id).await;
        let tasks = session.tasks.list(Some(&session.id));
        for task in tasks.into_iter().filter(|task| task.status.is_terminal()) {
            let _ = session.store.upsert_task(&task).await;
        }
        for agent in &agents {
            let unresolved = match session
                .store
                .load_unresolved_message_admissions(&session.id, &agent.id())
                .await
            {
                Ok(unresolved) => unresolved,
                Err(error) => {
                    log::warn!(
                        "failed to load unresolved admissions for {}: {error}",
                        session.id
                    );
                    continue;
                }
            };
            for admission in unresolved {
                let _ = session
                    .store
                    .mark_message_admission_terminal(
                        &session.id,
                        &admission.message_id,
                        Some("interrupted"),
                    )
                    .await;
            }
        }
        {
            let mut sessions = self
                .sessions
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if sessions
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(id);
            }
        }
        self.leases.forget(id);
        self.user_message_ids.write().await.remove(id);
    }

    /// Close every session, stop the router, shut down tasks and MCP servers, close
    /// the store.
    pub async fn shutdown(&self) {
        self.closing.store(true, AtomicOrdering::SeqCst);
        let ids = self.session_ids();
        for id in ids {
            self.close_session(&id).await;
        }
        self.deps.tasks.shutdown().await;
        tokio::task::yield_now().await;
        let router_handle = self
            .task_router
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(handle) = router_handle {
            handle.abort();
            let _ = handle.await;
        }
        self.deps.mcp.shutdown().await;
        self.deps.store.clone().close().await;
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
