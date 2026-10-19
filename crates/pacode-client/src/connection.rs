//! `Client`: one connection with reconnect.

mod reconnect;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pacode_types::ids::ClientId;
use pacode_types::model::ModelInfo;
use pacode_types::state::SessionMeta;
use pacode_types::{
    AgentId, Attach, AttachEnvelope, ClientHello, Envelope, McpServerInfo, PROTOCOL_VERSION,
    PluginCommandOutcome, PluginInfo, Reply, Request, SessionId, SessionSnapshot,
};
use tokio::io::BufReader;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    AttachAttempt, ClientError, ClientEvent, ClientEventContext, ClientExt, ClientOptions,
    EventReceiver, EventSink, LifecycleState, lock,
};

/// The last `Subscribe` sent on this client (`agents`, `all`), or `None` when it never
/// narrowed its stream. Shared with the supervisor, which replays it on reconnect.
pub(crate) type DesiredInterest = Arc<Mutex<Option<(Vec<AgentId>, bool)>>>;

enum RequestScope {
    Unscoped,
    Session(ClientEventContext),
    Attach(AttachAttempt),
}

fn requires_session(req: &Request) -> bool {
    !matches!(
        req,
        Request::Hello(_)
            | Request::Attach(_)
            | Request::Detach
            | Request::CloseSession
            | Request::Shutdown { .. }
            | Request::Ping
            | Request::ListSessions { .. }
            | Request::RuntimeStats
            | Request::ListModels
            | Request::RefreshModels
            | Request::ListMcpServers
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
    )
}

pub(super) fn clear_unacknowledged_attach_state(
    lifecycle: &LifecycleState,
    attempt: &AttachAttempt,
) {
    let _transition = lock(&lifecycle.transition_lock);
    if lifecycle.connection_epoch.load(Ordering::Acquire) != attempt.connection_epoch
        || lifecycle.session_epoch.load(Ordering::Acquire) != attempt.session_epoch
    {
        return;
    }
    let mut desired_guard = lock(&lifecycle.desired_attach);
    let Some(desired) = desired_guard.as_mut() else {
        return;
    };
    if desired.request_id != attempt.request_id
        || desired.idempotency_key != attempt.idempotency_key
        || desired.session_epoch != attempt.session_epoch
        || desired.connection_epoch != attempt.connection_epoch
    {
        return;
    }
    desired.target = attempt.target.clone();
    drop(desired_guard);
    *lock(&lifecycle.attached_session) = None;
    *lock(&lifecycle.snapshot_seq) = 0;
    *lock(&lifecycle.last_session_seq) = 0;
}

struct PendingRequestGuard {
    id: u64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pending_attaches: Arc<Mutex<HashMap<u64, AttachAttempt>>>,
    lifecycle: Arc<LifecycleState>,
    attach: Option<AttachAttempt>,
    armed: bool,
}

impl PendingRequestGuard {
    fn new(
        id: u64,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
        pending_attaches: Arc<Mutex<HashMap<u64, AttachAttempt>>>,
        lifecycle: Arc<LifecycleState>,
        attach: Option<AttachAttempt>,
    ) -> Self {
        Self {
            id,
            pending,
            pending_attaches,
            lifecycle,
            attach,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        lock(&self.pending).remove(&self.id);
        lock(&self.pending_attaches).remove(&self.id);
        if let Some(attempt) = self.attach.as_ref() {
            clear_unacknowledged_attach_state(&self.lifecycle, attempt);
        }
    }
}

pub struct Client {
    opts: ClientOptions,
    client_id: ClientId,
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pending_attaches: Arc<Mutex<HashMap<u64, AttachAttempt>>>,
    session_gate: Arc<RwLock<()>>,
    writer_tx: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    connected: Arc<AtomicBool>,
    lifecycle: Arc<LifecycleState>,
    cancel_token: CancellationToken,
    interest: DesiredInterest,
    supervisor_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Client {
    /// Connect (spawning the daemon when allowed), send `Hello`, start the reader.
    /// The returned receiver yields `ClientEvent`s (the first is `Connected`).
    pub async fn connect(opts: ClientOptions) -> Result<(Client, EventReceiver), ClientError> {
        let socket_path = opts.socket_path();
        log::info!("connecting to daemon at {}", socket_path.display());
        let stream = match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(stream) => stream,
            Err(connect_err) => {
                if opts.spawn_daemon {
                    let exe = match &opts.exe {
                        Some(exe) => exe.clone(),
                        None => std::env::current_exe()
                            .map_err(|error| ClientError::Spawn(error.to_string()))?,
                    };
                    crate::daemon_ctl::spawn_daemon(
                        &exe,
                        &socket_path,
                        &opts.paths.daemon_log(),
                        &[],
                    )?;
                    if !crate::daemon_ctl::wait_for_socket(
                        &socket_path,
                        std::time::Duration::from_secs(3),
                    )
                    .await
                    {
                        return Err(ClientError::Connect {
                            path: socket_path,
                            source: std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "timed out waiting for daemon socket",
                            ),
                        });
                    }
                    tokio::net::UnixStream::connect(&socket_path)
                        .await
                        .map_err(|error| ClientError::Connect {
                            path: socket_path.clone(),
                            source: error,
                        })?
                } else {
                    return Err(ClientError::Connect {
                        path: socket_path,
                        source: connect_err,
                    });
                }
            }
        };

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let client_id = ClientId::generate();
        let cancel_token = CancellationToken::new();
        let hello = Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: client_id.clone(),
                app_version: opts.app_version.clone(),
                protocol: PROTOCOL_VERSION,
            }),
        };
        reconnect::write_handshake(&mut write_half, &hello, opts.request_timeout, &cancel_token)
            .await
            .map_err(reconnect::client_error)?;
        let (reply, buffered_events) =
            reconnect::read_reply_handshake(&mut reader, 1, opts.request_timeout, &cancel_token)
                .await
                .map_err(|failure| reconnect::client_error(failure.kind))?;
        let (daemon_version, pid, version_mismatch) = match reply {
            Reply::Hello {
                daemon_version,
                protocol,
                pid,
            } => {
                if protocol != PROTOCOL_VERSION {
                    log::error!(
                        "protocol version mismatch: daemon={protocol}, client={PROTOCOL_VERSION}"
                    );
                    return Err(ClientError::Protocol {
                        daemon: protocol,
                        client: PROTOCOL_VERSION,
                    });
                }
                let version_mismatch = daemon_version != opts.app_version;
                log::info!(
                    "connected to daemon: pid={pid}, daemon_version={daemon_version}, client_id={client_id}"
                );
                (daemon_version, pid, version_mismatch)
            }
            Reply::Error { message } => {
                log::warn!("daemon rejected hello: {message}");
                return Err(ClientError::Daemon(message));
            }
            other => {
                return Err(ClientError::UnexpectedReply {
                    request: "hello".into(),
                    reply: format!("{other:?}"),
                });
            }
        };

        let (events_tx, events_rx) = EventSink::channel(cancel_token.clone());
        let dropped_events = Arc::new(AtomicUsize::new(0));
        let lifecycle = Arc::new(LifecycleState::new(1));
        if buffered_events.overflowed() {
            return Err(ClientError::UnexpectedReply {
                request: "hello".into(),
                reply: "event buffer overflow".into(),
            });
        }
        let buffered_events = buffered_events.into_events();
        reconnect::emit_event(
            &events_tx,
            &dropped_events,
            ClientEvent::Connected {
                context: lifecycle.context(),
                daemon_version,
                pid,
                version_mismatch,
            },
        )
        .await;
        for event in buffered_events {
            match event {
                reconnect::BufferedServerEvent::Event { seq, event } => {
                    if !event.is_global() {
                        continue;
                    }
                    if let Some(context) = lifecycle.accepted_event_context(seq, event.as_ref()) {
                        reconnect::emit_event(
                            &events_tx,
                            &dropped_events,
                            ClientEvent::Event {
                                context,
                                seq,
                                event: *event,
                            },
                        )
                        .await;
                    }
                }
                reconnect::BufferedServerEvent::Resync { current_seq } => {
                    if let Some(context) = lifecycle.accepted_resync_context(current_seq) {
                        events_tx.emit_resync(context, current_seq);
                    }
                }
            }
        }

        let writer =
            reconnect::spawn_writer(write_half, cancel_token.clone(), opts.request_timeout);
        let writer_tx_holder = Arc::new(Mutex::new(Some(writer.sender())));
        let next_id = Arc::new(AtomicU64::new(2));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_attaches = Arc::new(Mutex::new(HashMap::new()));
        let session_gate = Arc::new(RwLock::new(()));
        let connected = Arc::new(AtomicBool::new(true));
        let interest: DesiredInterest = Arc::new(Mutex::new(None));
        let supervisor_handle = tokio::spawn(reconnect::run_supervisor(reconnect::SupervisorCtx {
            reader,
            socket_path,
            client_id: client_id.clone(),
            opts: opts.clone(),
            next_id: next_id.clone(),
            pending: pending.clone(),
            pending_attaches: pending_attaches.clone(),
            events_tx,
            dropped_events,
            writer_tx_holder: writer_tx_holder.clone(),
            writer: Some(writer),
            connected: connected.clone(),
            lifecycle: lifecycle.clone(),
            cancel_token: cancel_token.clone(),
            interest: interest.clone(),
        }));

        Ok((
            Client {
                opts,
                client_id,
                next_id,
                pending,
                pending_attaches,
                session_gate,
                writer_tx: writer_tx_holder,
                connected,
                lifecycle,
                cancel_token,
                interest,
                supervisor_handle: Some(supervisor_handle),
            },
            events_rx,
        ))
    }

    pub fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    pub fn event_context(&self) -> ClientEventContext {
        self.lifecycle.context()
    }

    pub fn session_event_floor(&self) -> u64 {
        self.lifecycle.session_event_floor()
    }

    fn current_session(&self) -> Option<SessionId> {
        self.lifecycle.session()
    }

    fn begin_attach(&self, attach: Attach) -> AttachAttempt {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let idempotency_key = SessionId::generate().as_str().to_owned();
        self.lifecycle
            .begin_attach(attach, request_id, idempotency_key)
    }

    /// Attach to a session and return its snapshot; remembered for reconnects.
    pub async fn attach(&self, attach: Attach) -> Result<SessionSnapshot, ClientError> {
        let _guard = self.session_gate.write().await;
        let attempt = self.begin_attach(attach);
        self.attach_inner(attempt).await
    }

    pub async fn retry_attach(&self, attach: Attach) -> Result<SessionSnapshot, ClientError> {
        let _guard = self.session_gate.write().await;
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let Some(attempt) = self.lifecycle.retry_attach(&attach, request_id) else {
            return Err(ClientError::StaleSession);
        };
        log::info!("retrying attach: {:?}", attempt.target);
        self.attach_inner(attempt).await
    }

    async fn attach_inner(&self, attempt: AttachAttempt) -> Result<SessionSnapshot, ClientError> {
        log::info!("attaching to session: {:?}", attempt.target);
        let reply = self
            .request_inner(
                Request::Attach(attempt.target.clone()),
                RequestScope::Attach(attempt.clone()),
            )
            .await?;
        match reply {
            Reply::Attached(snapshot) => {
                if !self
                    .lifecycle
                    .is_attach_current(&attempt, &snapshot.meta.id)
                {
                    return Err(ClientError::StaleSession);
                }
                log::info!("attached to session {}", snapshot.meta.id);
                Ok(snapshot)
            }
            Reply::Error { message } => {
                self.clear_failed_attach(&attempt);
                log::warn!("failed to attach to session: {message}");
                Err(ClientError::Daemon(message))
            }
            other => {
                self.clear_failed_attach(&attempt);
                Err(ClientError::UnexpectedReply {
                    request: "attach".into(),
                    reply: format!("{other:?}"),
                })
            }
        }
    }

    /// Detach and attach as one ordered transition on the connection.
    pub async fn switch_session(&self, attach: Attach) -> Result<SessionSnapshot, ClientError> {
        let _guard = self.session_gate.write().await;
        let attempt = self.begin_attach(attach);
        let detach = self
            .request_inner(Request::Detach, RequestScope::Unscoped)
            .await;
        match detach {
            Ok(Reply::Ok) => {}
            Ok(Reply::Error { message }) => {
                self.clear_failed_attach(&attempt);
                return Err(ClientError::Daemon(message));
            }
            Ok(other) => {
                self.clear_failed_attach(&attempt);
                return Err(ClientError::UnexpectedReply {
                    request: "detach".into(),
                    reply: format!("{other:?}"),
                });
            }
            Err(error) => return Err(error),
        }
        self.attach_inner(attempt).await
    }

    pub fn session(&self) -> Option<SessionId> {
        self.current_session()
    }

    /// Send a request and await its reply (with `request_timeout`).
    pub async fn request(&self, req: Request) -> Result<Reply, ClientError> {
        match req {
            Request::Attach(attach) => self.attach(attach).await.map(Reply::Attached),
            Request::Detach => self.detach().await,
            Request::Hello(_) => Err(ClientError::InvalidSequenceRequest { request: "hello" }),
            req if requires_session(&req) => self.request_session(req).await,
            req => self.request_inner(req, RequestScope::Unscoped).await,
        }
    }

    async fn detach(&self) -> Result<Reply, ClientError> {
        let _guard = self.session_gate.write().await;
        self.lifecycle.begin_detach();
        self.request_inner(Request::Detach, RequestScope::Unscoped)
            .await
    }

    pub async fn close_session(&self) -> Result<Reply, ClientError> {
        let _guard = self.session_gate.write().await;
        let context = self.lifecycle.context();
        if context.session.is_none() {
            return Err(ClientError::StaleSession);
        }
        let reply = self
            .request_inner(Request::CloseSession, RequestScope::Session(context))
            .await?;
        if matches!(reply, Reply::Ok) {
            self.lifecycle.begin_detach();
        }
        Ok(reply)
    }

    pub async fn request_session_for(
        &self,
        context: ClientEventContext,
        req: Request,
    ) -> Result<Reply, ClientError> {
        if !requires_session(&req) {
            return Err(ClientError::InvalidSequenceRequest {
                request: req.name(),
            });
        }
        if context.session.is_none() {
            return Err(ClientError::StaleSession);
        }
        let _guard = self.session_gate.read().await;
        self.request_inner(req, RequestScope::Session(context))
            .await
    }

    pub async fn request_sequence<I>(
        &self,
        context: ClientEventContext,
        requests: I,
    ) -> Result<Vec<Reply>, ClientError>
    where
        I: IntoIterator<Item = Request>,
    {
        if context.session.is_none() {
            return Err(ClientError::StaleSession);
        }
        let requests = requests.into_iter().collect::<Vec<_>>();
        let _guard = self.session_gate.read().await;
        if !self.lifecycle.is_context_current(&context) {
            return Err(ClientError::StaleSession);
        }
        if let Some(request) = requests.iter().find(|request| !requires_session(request)) {
            return Err(ClientError::InvalidSequenceRequest {
                request: request.name(),
            });
        }
        let mut replies = Vec::with_capacity(requests.len());
        for request in requests {
            let reply = self
                .request_inner(request, RequestScope::Session(context.clone()))
                .await?;
            match reply {
                Reply::Error { message } => return Err(ClientError::Daemon(message)),
                reply => replies.push(reply),
            }
        }
        Ok(replies)
    }

    /// Send a session-bound request while holding the session transition gate.
    pub async fn request_session(&self, req: Request) -> Result<Reply, ClientError> {
        let context = self.event_context();
        self.request_session_for(context, req).await
    }

    fn clear_failed_attach(&self, attempt: &AttachAttempt) {
        let _ = self.lifecycle.clear_failed_attach(attempt);
    }

    async fn request_inner(&self, req: Request, scope: RequestScope) -> Result<Reply, ClientError> {
        if !self.is_connected() {
            return Err(ClientError::Disconnected);
        }
        let attach_metadata = match &scope {
            RequestScope::Attach(attempt) => Some(attempt.clone()),
            RequestScope::Unscoped | RequestScope::Session(_) => None,
        };
        let id = attach_metadata
            .as_ref()
            .map(|attempt| attempt.request_id)
            .unwrap_or_else(|| self.next_id.fetch_add(1, Ordering::Relaxed));
        let req_name = req.name();
        let (tx, rx) = oneshot::channel();
        let mut pending_guard = PendingRequestGuard::new(
            id,
            self.pending.clone(),
            self.pending_attaches.clone(),
            self.lifecycle.clone(),
            attach_metadata.clone(),
        );
        lock(&self.pending).insert(id, tx);
        if let Some(attempt) = &attach_metadata {
            lock(&self.pending_attaches).insert(id, attempt.clone());
        }

        let subscribed = match &req {
            Request::Subscribe { agents, all } => Some((agents.clone(), *all)),
            _ => None,
        };
        let envelope = Envelope { id, req };
        let serialized = match attach_metadata.as_ref() {
            Some(attempt) => serde_json::to_string(&AttachEnvelope {
                envelope,
                idempotency_key: Some(attempt.idempotency_key.clone()),
            }),
            None => serde_json::to_string(&envelope),
        };
        let mut line = match serialized {
            Ok(line) => line,
            Err(error) => return Err(ClientError::Json(error)),
        };
        line.push('\n');
        let writer = lock(&self.writer_tx).clone();
        let Some(writer) = writer else {
            return Err(ClientError::Disconnected);
        };
        let deadline = tokio::time::Instant::now() + self.opts.request_timeout;
        let permit = tokio::select! {
            _ = self.cancel_token.cancelled() => {
                return Err(ClientError::Disconnected);
            }
            result = tokio::time::timeout_at(deadline, writer.reserve()) => {
                match result {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_)) => {
                        return Err(ClientError::Disconnected);
                    }
                    Err(_) => {
                        return Err(ClientError::Timeout(req_name));
                    }
                }
            }
        };
        {
            let _transition = lock(&self.lifecycle.transition_lock);
            let current = match &scope {
                RequestScope::Unscoped => true,
                RequestScope::Session(expected) => self.lifecycle.context_unlocked() == *expected,
                RequestScope::Attach(attempt) => {
                    self.lifecycle.outgoing_attach_is_current_unlocked(attempt)
                }
            };
            if !current {
                drop(permit);
                return Err(ClientError::StaleSession);
            }
            if let Some(subscribed) = subscribed {
                // Recorded as it is sent: a reconnect racing the reply still replays
                // the newest interest, not the one the lost connection had.
                *lock(&self.interest) = Some(subscribed);
            }
            permit.send(line);
        }

        match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(reply)) => {
                if let Reply::Snapshot(snapshot) = &reply {
                    let _ = self.lifecycle.observe_snapshot(snapshot);
                }
                pending_guard.disarm();
                Ok(reply)
            }
            Ok(Err(_)) => Err(ClientError::Disconnected),
            Err(_) => Err(ClientError::Timeout(req_name)),
        }
    }

    /// `request` expecting `Reply::Ok`; `Reply::Error` becomes `ClientError::Daemon`.
    pub async fn ok(&self, req: Request) -> Result<(), ClientError> {
        match self.request(req).await? {
            Reply::Ok => Ok(()),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "ok".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// List MCP servers and their status.
    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerInfo>, ClientError> {
        match self.request(Request::ListMcpServers).await? {
            Reply::McpServers { servers } => Ok(servers),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "list_mcp_servers".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// Restart one MCP server.
    pub async fn restart_mcp_server(&self, server: impl Into<String>) -> Result<(), ClientError> {
        self.ok(Request::RestartMcpServer {
            server: server.into(),
        })
        .await
    }

    /// Enable or disable one MCP server.
    pub async fn set_mcp_server_enabled(
        &self,
        server: impl Into<String>,
        enabled: bool,
    ) -> Result<(), ClientError> {
        self.ok(Request::SetMcpServerEnabled {
            server: server.into(),
            enabled,
        })
        .await
    }

    /// Render an MCP prompt.
    pub async fn get_mcp_prompt(
        &self,
        server: impl Into<String>,
        name: impl Into<String>,
        args: BTreeMap<String, String>,
    ) -> Result<String, ClientError> {
        match self
            .request(Request::GetMcpPrompt {
                server: server.into(),
                name: name.into(),
                args,
            })
            .await?
        {
            Reply::McpPrompt { text } => Ok(text),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "get_mcp_prompt".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// List loaded plugins.
    pub async fn list_plugins(&self) -> Result<Vec<PluginInfo>, ClientError> {
        match self.request(Request::ListPlugins).await? {
            Reply::Plugins { plugins } => Ok(plugins),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "list_plugins".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// Run a plugin slash command.
    pub async fn run_plugin_command(
        &self,
        name: impl Into<String>,
        args: impl Into<String>,
    ) -> Result<PluginCommandOutcome, ClientError> {
        match self
            .request(Request::RunPluginCommand {
                name: name.into(),
                args: args.into(),
            })
            .await?
        {
            Reply::PluginCommand(outcome) => Ok(outcome),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "run_plugin_command".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// List recent sessions.
    pub async fn list_sessions(&self, limit: u32) -> Result<Vec<SessionMeta>, ClientError> {
        match self.request(Request::ListSessions { limit }).await? {
            Reply::Sessions { sessions } => Ok(sessions),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "list_sessions".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// List available models.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, ClientError> {
        match self.request(Request::ListModels).await? {
            Reply::Models { models } => Ok(models),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "list_models".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Stop the reader/reconnect loop and close the socket.
    pub async fn close(mut self) {
        log::info!("closing client connection (client_id={})", self.client_id);
        self.cancel_token.cancel();
        self.connected.store(false, Ordering::Relaxed);
        *lock(&self.writer_tx) = None;
        reconnect::fail_pending(&self.pending, &self.pending_attaches);
        if let Some(handle) = self.supervisor_handle.take() {
            let _ = handle.await;
        }
    }
}

impl ClientExt for Client {
    fn attached_session(&self) -> Option<SessionId> {
        self.session()
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("client_id", &self.client_id)
            .field("connected", &self.is_connected())
            .field("session", &self.session())
            .finish()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn committed_attach_timeout_preserves_retry_state() {
        let lifecycle = LifecycleState::new(1);
        let session = SessionId::new("ses_test");
        let attempt = lifecycle.begin_attach(
            Attach::New {
                cwd: PathBuf::from("/tmp"),
                model: None,
                effort: None,
                mode: None,
            },
            2,
            "key".into(),
        );
        *lock(&lifecycle.desired_attach) = Some(AttachAttempt {
            request_id: attempt.request_id,
            idempotency_key: attempt.idempotency_key.clone(),
            target: Attach::Resume {
                session: session.clone(),
            },
            session_epoch: attempt.session_epoch,
            connection_epoch: attempt.connection_epoch,
        });
        *lock(&lifecycle.attached_session) = Some(session.clone());

        clear_unacknowledged_attach_state(&lifecycle, &attempt);

        assert_eq!(lifecycle.desired_attach().unwrap().target, attempt.target);
        assert!(lifecycle.session().is_none());
        let retry = lifecycle.retry_attach(&attempt.target, 3).unwrap();
        assert_eq!(retry.idempotency_key, attempt.idempotency_key);
    }

    #[test]
    fn dropping_pending_request_removes_waiter_and_rolls_back_attach() {
        let lifecycle = Arc::new(LifecycleState::new(1));
        let attempt = lifecycle.begin_attach(
            Attach::New {
                cwd: PathBuf::from("/tmp"),
                model: None,
                effort: None,
                mode: None,
            },
            2,
            "key".into(),
        );
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_attaches = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = oneshot::channel();
        lock(&pending).insert(attempt.request_id, tx);
        lock(&pending_attaches).insert(attempt.request_id, attempt.clone());
        let guard = PendingRequestGuard::new(
            attempt.request_id,
            pending.clone(),
            pending_attaches.clone(),
            lifecycle.clone(),
            Some(attempt.clone()),
        );

        drop(guard);

        assert!(lock(&pending).is_empty());
        assert!(lock(&pending_attaches).is_empty());
        assert_eq!(lifecycle.desired_attach().unwrap().target, attempt.target);
        assert!(lifecycle.session().is_none());
    }
}
