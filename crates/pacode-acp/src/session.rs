//! Session management and execution for pacode ACP.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock};

use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, CloseSessionResponse, ConfigOptionUpdate,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
    ResumeSessionResponse, SessionConfigOption, SessionInfo, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    SetSessionModeResponse, StopReason as AcpStopReason,
};
use agent_client_protocol::{Client as AcpClient, ConnectionTo, Responder};
use pacode_client::{Client, ClientEvent, ClientOptions, EventReceiver};
use pacode_types::Attach;
use pacode_types::ids::SessionId as PacodeSessionId;
use pacode_types::model::ModelInfo;
use pacode_types::protocol::{Event, Request};
use pacode_types::state::{Mode, SessionMeta};

use crate::config_options::{
    self, ConfigOptionCaps, SessionConfigState, apply_config_value, session_config_options,
};
use crate::error::AcpError;
use crate::mapping::{
    MappingState, event_to_session_updates, modes_state, permission_request_to_acp,
    permission_response_to_decision, plan_to_acp, prompt_content_to_parts, session_id_from_acp,
    session_id_to_acp, transcript_item_to_session_updates, turn_stop_to_stop_reason,
};

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

/// Position in `order` of the least recently used session that may be dropped,
/// or `None` when every one of them has a turn in flight. Kept out of the
/// manager so the choice can be tested without a daemon connection.
fn evict_candidate(
    order: &VecDeque<PacodeSessionId>,
    is_busy: impl Fn(&PacodeSessionId) -> bool,
) -> Option<usize> {
    order.iter().position(|candidate| !is_busy(candidate))
}

/// Move `id` to the most-recently-used end of `order`, if it is there at all.
fn touch_order(order: &mut VecDeque<PacodeSessionId>, id: &PacodeSessionId) {
    if let Some(pos) = order.iter().position(|existing| existing == id) {
        order.remove(pos);
        order.push_back(id.clone());
    }
}

/// An active session connected to the pacode daemon.
pub struct SessionState {
    pub id: PacodeSessionId,
    pub client: Arc<Client>,
    pub events: Mutex<EventReceiver>,
    /// Last known meta + model catalog backing the ACP config options.
    pub config: Mutex<SessionConfigState>,
    /// Config-option features the client advertised at initialize.
    pub config_caps: ConfigOptionCaps,
}

impl SessionState {
    pub fn new(
        id: PacodeSessionId,
        client: Client,
        events: EventReceiver,
        config: SessionConfigState,
        config_caps: ConfigOptionCaps,
    ) -> Self {
        Self {
            id,
            client: Arc::new(client),
            events: Mutex::new(events),
            config: Mutex::new(config),
            config_caps,
        }
    }

    /// Whether a prompt turn is in flight. `run_prompt` holds the event-stream
    /// lock for the whole turn, so the lock itself is the marker; there is no
    /// separate flag that could drift out of sync with it.
    pub fn is_busy(&self) -> bool {
        self.events.try_lock().is_err()
    }

    /// Sends an interrupt request to the daemon.
    pub async fn interrupt(&self) -> Result<(), AcpError> {
        self.client.ok(Request::Interrupt).await?;
        Ok(())
    }

    /// Runs a prompt turn, streaming updates to the client and handling permissions.
    pub async fn run_prompt(
        &self,
        prompt: &PromptRequest,
        cx: &ConnectionTo<AcpClient>,
    ) -> Result<AcpStopReason, AcpError> {
        let (text, images) = prompt_content_to_parts(&prompt.prompt);
        let acp_session_id = prompt.session_id.clone();

        let mut events = self.events.lock().await;

        // Submit the user message to the pacode daemon
        self.client
            .ok(Request::UserMessage { text, images })
            .await?;

        let mut mapping_state = MappingState::new();

        while let Some(client_ev) = events.recv().await {
            match client_ev {
                ClientEvent::Event { event, .. } => {
                    if let Event::PermissionRequested(perm) = &event {
                        let req = permission_request_to_acp(&acp_session_id, perm);
                        let resp = cx
                            .send_request(req)
                            .block_task()
                            .await
                            .map_err(AcpError::Protocol)?;
                        let decision = permission_response_to_decision(&resp);
                        self.client
                            .ok(Request::PermissionReply {
                                permission: perm.id.clone(),
                                decision,
                                // ACP deny/cancel carries no note: a plain
                                // user refusal.
                                reason: None,
                            })
                            .await?;
                        continue;
                    }

                    if let Event::TurnEnded { stop, .. } = &event {
                        let stop_reason = turn_stop_to_stop_reason(stop);
                        return Ok(stop_reason);
                    }

                    let config_update = self.config_update(&event).await;
                    let updates = event_to_session_updates(&event, &mut mapping_state);
                    for update in updates.into_iter().chain(config_update) {
                        cx.send_notification(SessionNotification::new(
                            acp_session_id.clone(),
                            update,
                        ))
                        .map_err(AcpError::Protocol)?;
                    }
                }
                ClientEvent::Snapshot(snapshot) => {
                    // Re-attached after a reconnect: the snapshot replaces all
                    // client state, so the cached meta follows it.
                    let update = {
                        let mut config = self.config.lock().await;
                        config.meta = snapshot.meta.clone();
                        config_options_update(&config, self.config_caps)
                    };
                    if let Some(update) = update {
                        cx.send_notification(SessionNotification::new(
                            acp_session_id.clone(),
                            update,
                        ))
                        .map_err(AcpError::Protocol)?;
                    }
                }
                ClientEvent::Connected { .. } => {}
                ClientEvent::Disconnected { reason } => {
                    log::warn!("daemon disconnected: {reason}");
                }
                ClientEvent::Reconnecting { attempt } => {
                    log::info!("reconnecting to daemon, attempt {attempt}");
                }
            }
        }

        // Stream closed before TurnEnded
        Ok(AcpStopReason::EndTurn)
    }

    /// Folds `event` into the config-option cache; when the client advertised
    /// config-option support, returns the rebuilt list to notify about.
    async fn config_update(&self, event: &Event) -> Option<SessionUpdate> {
        let mut config = self.config.lock().await;
        if !config_options::apply_event(&mut config, event) {
            return None;
        }
        config_options_update(&config, self.config_caps)
    }
}

/// Rebuilds the config-option list as a `ConfigOptionUpdate` when the client
/// advertised support; `None` otherwise.
fn config_options_update(
    config: &SessionConfigState,
    caps: ConfigOptionCaps,
) -> Option<SessionUpdate> {
    caps.select.then(|| {
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(session_config_options(
            &config.meta,
            &config.models,
            caps,
        )))
    })
}

/// Manages active ACP sessions connected to the pacode daemon.
///
/// Memory cap: holds at most `MAX_SESSIONS` (64) concurrent sessions in memory.
/// Invalidation: `order` is a true LRU queue — a session moves to the back both
/// when it is inserted and when it is looked up — and the least recently used
/// idle session is dropped once the cap is reached.
/// Cleanup: Dropping `SessionState` drops the `Client`, closing the socket connection.
pub struct SessionManager {
    client_opts: ClientOptions,
    sessions: RwLock<HashMap<PacodeSessionId, Arc<SessionState>>>,
    order: Mutex<VecDeque<PacodeSessionId>>,
    max_sessions: usize,
    /// `session.config_options` advertised at initialize: when false, session
    /// responses carry `modes` only and the model catalog is never fetched.
    config_options_supported: AtomicBool,
    /// `config_options.boolean` sub-capability: controls whether the ZDR
    /// option is emitted as a toggle or a two-value select.
    boolean_options_supported: AtomicBool,
}

const MAX_SESSIONS: usize = 64;

impl SessionManager {
    pub fn new(client_opts: ClientOptions) -> Self {
        Self {
            client_opts,
            sessions: RwLock::new(HashMap::new()),
            order: Mutex::new(VecDeque::new()),
            max_sessions: MAX_SESSIONS,
            config_options_supported: AtomicBool::new(false),
            boolean_options_supported: AtomicBool::new(false),
        }
    }

    /// Stores the config-option capabilities negotiated at initialize.
    /// Initialize arrives once before any session request, so relaxed
    /// ordering is enough.
    pub fn set_config_caps(&self, caps: ConfigOptionCaps) {
        self.config_options_supported
            .store(caps.select, Ordering::Relaxed);
        self.boolean_options_supported
            .store(caps.boolean, Ordering::Relaxed);
    }

    fn config_caps(&self) -> ConfigOptionCaps {
        ConfigOptionCaps {
            select: self.config_options_supported.load(Ordering::Relaxed),
            boolean: self.boolean_options_supported.load(Ordering::Relaxed),
        }
    }

    /// Fetches the model catalog for the config options — only when the
    /// client uses them. Errors degrade to an empty list (the picker then
    /// shows just the current route) and never fail the session attach.
    async fn fetch_models(&self, client: &Client) -> Vec<ModelInfo> {
        if !self.config_options_supported.load(Ordering::Relaxed) {
            return Vec::new();
        }
        match client.list_models().await {
            Ok(models) => models,
            Err(e) => {
                log::warn!(
                    "list_models failed; model picker will show the current route only: {e}"
                );
                Vec::new()
            }
        }
    }

    /// Builds a `SessionState` around a freshly attached client: populates
    /// the config-option cache and returns the initial option list to embed
    /// in the session response when the client advertised support.
    async fn build_state(
        &self,
        id: PacodeSessionId,
        client: Client,
        events: EventReceiver,
        meta: &SessionMeta,
    ) -> (Arc<SessionState>, Option<Vec<SessionConfigOption>>) {
        let caps = self.config_caps();
        let models = self.fetch_models(&client).await;
        let options = caps
            .select
            .then(|| session_config_options(meta, &models, caps));
        let session = Arc::new(SessionState::new(
            id,
            client,
            events,
            SessionConfigState {
                meta: meta.clone(),
                models,
            },
            caps,
        ));
        (session, options)
    }

    /// Inserts a session, evicting the least recently used one at capacity.
    ///
    /// A session with a turn in flight is never evicted: dropping it would close
    /// the daemon socket mid-turn and the editor would see the answer stop with
    /// no explanation. If every candidate is busy the map is left one over the
    /// cap instead, which is the lesser problem and is logged.
    async fn insert(&self, id: PacodeSessionId, session: Arc<SessionState>) {
        let mut order = self.order.lock().await;
        let mut sessions = self.sessions.write().await;

        if sessions.len() >= self.max_sessions && !sessions.contains_key(&id) {
            let victim = evict_candidate(&order, |candidate| {
                sessions.get(candidate).is_some_and(|state| state.is_busy())
            });
            match victim {
                Some(pos) => {
                    if let Some(evicted) = order.remove(pos) {
                        sessions.remove(&evicted);
                        log::debug!("evicted least recently used ACP session {evicted}");
                    }
                }
                None => log::warn!(
                    "all {} ACP sessions have a turn in flight; keeping one over the cap",
                    self.max_sessions
                ),
            }
        }

        order.retain(|existing| existing != &id);
        order.push_back(id.clone());
        sessions.insert(id, session);
    }

    /// Marks `id` as most recently used.
    async fn touch(&self, id: &PacodeSessionId) {
        let mut order = self.order.lock().await;
        touch_order(&mut order, id);
    }

    /// Gets an existing session or resumes it from the daemon if known.
    pub async fn get_or_resume(&self, id: &PacodeSessionId) -> Result<Arc<SessionState>, AcpError> {
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(id) {
                let session = Arc::clone(session);
                drop(sessions);
                self.touch(id).await;
                return Ok(session);
            }
        }

        // Attempt to connect and attach to the session on the daemon
        let (client, events) = Client::connect(self.client_opts.clone()).await?;
        let snapshot = client
            .attach(Attach::Resume {
                session: id.clone(),
            })
            .await?;

        let (session, _options) = self
            .build_state(id.clone(), client, events, &snapshot.meta)
            .await;
        self.insert(id.clone(), Arc::clone(&session)).await;
        Ok(session)
    }

    /// Handles `session/new`.
    pub async fn new_session(
        &self,
        req: NewSessionRequest,
    ) -> Result<NewSessionResponse, AcpError> {
        let (client, events) = Client::connect(self.client_opts.clone()).await?;
        let snapshot = client
            .attach(Attach::New {
                cwd: req.cwd,
                model: None,
                effort: None,
                mode: None,
            })
            .await?;

        let session_id = snapshot.meta.id.clone();
        let acp_id = session_id_to_acp(&session_id);
        let (session, config_options) = self
            .build_state(session_id.clone(), client, events, &snapshot.meta)
            .await;
        self.insert(session_id, session).await;

        Ok(NewSessionResponse::new(acp_id)
            .modes(modes_state(snapshot.meta.mode))
            .config_options(config_options))
    }

    /// Handles `session/load`.
    pub async fn load_session(
        &self,
        req: LoadSessionRequest,
        cx: &ConnectionTo<AcpClient>,
    ) -> Result<LoadSessionResponse, AcpError> {
        let pacode_id = session_id_from_acp(&req.session_id);
        let (client, events) = Client::connect(self.client_opts.clone()).await?;
        let snapshot = client
            .attach(Attach::Resume {
                session: pacode_id.clone(),
            })
            .await?;

        // Replay historical transcript items
        for item in &snapshot.transcript {
            for update in transcript_item_to_session_updates(item) {
                cx.send_notification(SessionNotification::new(req.session_id.clone(), update))
                    .map_err(AcpError::Protocol)?;
            }
        }

        // Replay plan if present
        if !snapshot.plan.is_empty() {
            cx.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::Plan(plan_to_acp(&snapshot.plan)),
            ))
            .map_err(AcpError::Protocol)?;
        }

        let (session, config_options) = self
            .build_state(pacode_id.clone(), client, events, &snapshot.meta)
            .await;
        self.insert(pacode_id, session).await;

        Ok(LoadSessionResponse::new()
            .modes(modes_state(snapshot.meta.mode))
            .config_options(config_options))
    }

    /// Handles `session/resume`.
    pub async fn resume_session(
        &self,
        req: ResumeSessionRequest,
        cx: &ConnectionTo<AcpClient>,
    ) -> Result<ResumeSessionResponse, AcpError> {
        let pacode_id = session_id_from_acp(&req.session_id);
        let (client, events) = Client::connect(self.client_opts.clone()).await?;
        let snapshot = client
            .attach(Attach::Resume {
                session: pacode_id.clone(),
            })
            .await?;

        // Replay transcript items
        for item in &snapshot.transcript {
            for update in transcript_item_to_session_updates(item) {
                cx.send_notification(SessionNotification::new(req.session_id.clone(), update))
                    .map_err(AcpError::Protocol)?;
            }
        }

        if !snapshot.plan.is_empty() {
            cx.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::Plan(plan_to_acp(&snapshot.plan)),
            ))
            .map_err(AcpError::Protocol)?;
        }

        let (session, config_options) = self
            .build_state(pacode_id.clone(), client, events, &snapshot.meta)
            .await;
        self.insert(pacode_id, session).await;

        Ok(ResumeSessionResponse::new()
            .modes(modes_state(snapshot.meta.mode))
            .config_options(config_options))
    }

    /// Handles `session/prompt`.
    pub async fn handle_prompt(
        &self,
        req: PromptRequest,
        responder: Responder<PromptResponse>,
        cx: ConnectionTo<AcpClient>,
    ) {
        let pacode_id = session_id_from_acp(&req.session_id);
        let session = match self.get_or_resume(&pacode_id).await {
            Ok(s) => s,
            Err(e) => {
                let _ =
                    responder.respond_with_internal_error(format!("failed to attach session: {e}"));
                return;
            }
        };

        match session.run_prompt(&req, &cx).await {
            Ok(stop_reason) => {
                let _ = responder.respond(PromptResponse::new(stop_reason));
            }
            Err(e) => {
                let _ = responder.respond_with_internal_error(format!("prompt turn failed: {e}"));
            }
        }
    }

    /// Handles `session/setMode`.
    pub async fn set_mode(
        &self,
        req: SetSessionModeRequest,
    ) -> Result<SetSessionModeResponse, AcpError> {
        let mode_str = &req.mode_id.0;
        let mode = Mode::parse(mode_str.as_ref())
            .ok_or_else(|| AcpError::Internal(format!("unknown mode: {mode_str}")))?;

        let pacode_id = session_id_from_acp(&req.session_id);
        let session = self.get_or_resume(&pacode_id).await?;
        session.client.ok(Request::SetMode(mode)).await?;

        Ok(SetSessionModeResponse::new())
    }

    /// Handles `session/set_config_option`.
    ///
    /// Sends the validated change to the daemon, folds it into the cached
    /// meta on success and answers with the full rebuilt option list, as the
    /// method contract requires.
    pub async fn set_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> Result<SetSessionConfigOptionResponse, AcpError> {
        let change = apply_config_value(req.config_id.0.as_ref(), &req.value)?;
        let pacode_id = session_id_from_acp(&req.session_id);
        let session = self.get_or_resume(&pacode_id).await?;
        session.client.ok(change.to_request()).await?;

        let mut config = session.config.lock().await;
        change.apply_to_meta(&mut config.meta);
        Ok(SetSessionConfigOptionResponse::new(session_config_options(
            &config.meta,
            &config.models,
            session.config_caps,
        )))
    }

    /// Handles `session/list`.
    pub async fn list_sessions(
        &self,
        req: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, AcpError> {
        // Use an existing client if available, or create a temporary connection
        let client_opt = {
            let sessions = self.sessions.read().await;
            sessions.values().next().map(|s| Arc::clone(&s.client))
        };

        let metas = if let Some(client) = client_opt {
            client.list_sessions(50).await?
        } else {
            let (client, _rx) = Client::connect(self.client_opts.clone()).await?;
            client.list_sessions(50).await?
        };

        let mut session_infos = Vec::new();
        for meta in metas {
            if let Some(ref filter_cwd) = req.cwd
                && &meta.cwd != filter_cwd
            {
                continue;
            }

            let acp_id = session_id_to_acp(&meta.id);
            let title = meta.title();
            let mut info = SessionInfo::new(acp_id, meta.cwd);
            if !title.is_empty() {
                info = info.title(title);
            }
            if meta.updated_at_ms > 0
                && let Some(dt) = chrono::DateTime::from_timestamp_millis(meta.updated_at_ms as i64)
            {
                info = info.updated_at(dt.to_rfc3339());
            }
            session_infos.push(info);
        }

        Ok(ListSessionsResponse::new(session_infos))
    }

    /// Handles `session/close`.
    pub async fn close_session(
        &self,
        req: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, AcpError> {
        let pacode_id = session_id_from_acp(&req.session_id);
        {
            let mut order = self.order.lock().await;
            order.retain(|id| id != &pacode_id);
            let mut sessions = self.sessions.write().await;
            sessions.remove(&pacode_id);
        }
        Ok(CloseSessionResponse::new())
    }

    /// Handles `session/cancel`.
    pub async fn handle_cancel(&self, notif: CancelNotification) {
        let pacode_id = session_id_from_acp(&notif.session_id);
        let session_opt = {
            let sessions = self.sessions.read().await;
            sessions.get(&pacode_id).cloned()
        };

        if let Some(session) = session_opt
            && let Err(e) = session.interrupt().await
        {
            log::warn!("failed to interrupt session {pacode_id}: {e}");
        }
    }
}
