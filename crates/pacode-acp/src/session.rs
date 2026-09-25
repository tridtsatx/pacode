//! Session management and execution for pacode ACP.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, RwLock, broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, CloseSessionResponse, ConfigOptionUpdate,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
    ResumeSessionResponse, SessionConfigOption, SessionId as AcpSessionId, SessionInfo,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
    StopReason as AcpStopReason,
};
use agent_client_protocol::{Client as AcpClient, ConnectionTo, Responder};
use pacode_client::{
    Client, ClientError, ClientEvent, ClientEventContext, ClientOptions, EventReceiver,
};
use pacode_types::Attach;
use pacode_types::ids::{AgentId, ClientId, PermissionId, SessionId as PacodeSessionId, TurnId};
use pacode_types::model::ModelInfo;
use pacode_types::protocol::{Event, Reply, Request, SessionSnapshot};
use pacode_types::state::{AgentStatus, Mode, PermissionDecision, PermissionRequest, Plan};

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

const EVENT_CHANNEL_CAPACITY: usize = 256;
const HISTORY_PAGE_LIMIT: usize = 200;
const MAX_HISTORY_PAGES: usize = 32;
const MAX_HISTORY_REPLAY_BYTES: usize = 4 * 1024 * 1024;
const INITIAL_TRANSCRIPT_LIMIT: usize = 1024;
const SESSION_REQUEST_MAX_ATTEMPTS: usize = 3;
const RECONNECT_CONTEXT_TIMEOUT: Duration = Duration::from_secs(2);

struct EventPumpState {
    session_id: PacodeSessionId,
    connection_epoch: AtomicU64,
    session_epoch: AtomicU64,
    attach_failed: AtomicBool,
    closed: AtomicBool,
    prompt_active: AtomicBool,
    overflowed: AtomicBool,
    recovering: AtomicBool,
    notify: Notify,
    recovery_notify: Notify,
}

impl EventPumpState {
    fn finish_recovery(&self) {
        self.recovering.store(false, Ordering::Release);
        self.recovery_notify.notify_one();
    }
}

struct PromptActivationGuard {
    state: Arc<EventPumpState>,
}

impl Drop for PromptActivationGuard {
    fn drop(&mut self) {
        self.state.prompt_active.store(false, Ordering::Release);
        self.state.finish_recovery();
    }
}

fn spawn_event_pump(
    mut events: EventReceiver,
    sender: broadcast::Sender<ClientEvent>,
    prompt_sender: mpsc::Sender<ClientEvent>,
    session_id: PacodeSessionId,
    context: ClientEventContext,
) -> (JoinHandle<()>, Arc<EventPumpState>) {
    let state = Arc::new(EventPumpState {
        session_id,
        connection_epoch: AtomicU64::new(context.connection_epoch),
        session_epoch: AtomicU64::new(context.session_epoch),
        attach_failed: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        prompt_active: AtomicBool::new(false),
        overflowed: AtomicBool::new(false),
        recovering: AtomicBool::new(false),
        notify: Notify::new(),
        recovery_notify: Notify::new(),
    });
    let pump_state = Arc::clone(&state);
    let task = tokio::spawn(async move {
        loop {
            loop {
                if !pump_state.prompt_active.load(Ordering::Acquire)
                    && pump_state.recovering.load(Ordering::Acquire)
                {
                    pump_state.finish_recovery();
                }
                let notified = pump_state.recovery_notify.notified();
                if !pump_state.recovering.load(Ordering::Acquire) {
                    break;
                }
                notified.await;
            }
            let Some(event) = events.recv().await else {
                break;
            };
            match &event {
                ClientEvent::Connected { context, .. }
                    if context.session.as_ref() == Some(&pump_state.session_id) =>
                {
                    let current_connection = pump_state.connection_epoch.load(Ordering::Acquire);
                    let current_session = pump_state.session_epoch.load(Ordering::Acquire);
                    let newer = context.connection_epoch > current_connection
                        || (context.connection_epoch == current_connection
                            && context.session_epoch >= current_session);
                    let strictly_newer = context.connection_epoch > current_connection
                        || (context.connection_epoch == current_connection
                            && context.session_epoch > current_session);
                    if newer
                        && (!pump_state.attach_failed.load(Ordering::Acquire) || strictly_newer)
                    {
                        pump_state.attach_failed.store(false, Ordering::Release);
                        pump_state
                            .connection_epoch
                            .store(context.connection_epoch, Ordering::Release);
                        if context.session_epoch != 0 {
                            pump_state
                                .session_epoch
                                .store(context.session_epoch, Ordering::Release);
                        }
                    }
                }
                ClientEvent::Snapshot { context, snapshot }
                    if snapshot.meta.id == pump_state.session_id
                        && context.session.as_ref() == Some(&pump_state.session_id) =>
                {
                    let current_connection = pump_state.connection_epoch.load(Ordering::Acquire);
                    let current_session = pump_state.session_epoch.load(Ordering::Acquire);
                    let newer = context.connection_epoch > current_connection
                        || (context.connection_epoch == current_connection
                            && context.session_epoch >= current_session);
                    let strictly_newer = context.connection_epoch > current_connection
                        || (context.connection_epoch == current_connection
                            && context.session_epoch > current_session);
                    if newer
                        && (!pump_state.attach_failed.load(Ordering::Acquire) || strictly_newer)
                    {
                        pump_state.attach_failed.store(false, Ordering::Release);
                        pump_state
                            .connection_epoch
                            .store(context.connection_epoch, Ordering::Release);
                        if context.session_epoch != 0 {
                            pump_state
                                .session_epoch
                                .store(context.session_epoch, Ordering::Release);
                        }
                    }
                }
                ClientEvent::AttachFailed {
                    context, attach, ..
                } if context
                    .session
                    .as_ref()
                    .is_none_or(|session| session == &pump_state.session_id)
                    && matches!(&attach, Attach::Resume { session, .. } if session == &pump_state.session_id) =>
                {
                    let current_connection = pump_state.connection_epoch.load(Ordering::Acquire);
                    let current_session = pump_state.session_epoch.load(Ordering::Acquire);
                    if context.connection_epoch > current_connection
                        || (context.connection_epoch == current_connection
                            && context.session_epoch >= current_session)
                    {
                        pump_state
                            .connection_epoch
                            .store(context.connection_epoch, Ordering::Release);
                        if context.session_epoch != 0 {
                            pump_state
                                .session_epoch
                                .store(context.session_epoch, Ordering::Release);
                        }
                        pump_state.attach_failed.store(true, Ordering::Release);
                    }
                }
                _ => {}
            }
            if sender.receiver_count() > 0
                && matches!(
                    &event,
                    ClientEvent::Connected { .. }
                        | ClientEvent::Disconnected { .. }
                        | ClientEvent::Reconnecting { .. }
                        | ClientEvent::AttachFailed { .. }
                        | ClientEvent::ResyncNeeded { .. }
                )
            {
                let _ = sender.send(event.clone());
            }
            if !pump_state.prompt_active.load(Ordering::Acquire) {
                continue;
            }
            if matches!(&event, ClientEvent::ResyncNeeded { .. }) {
                pump_state.recovering.store(true, Ordering::Release);
            }
            if prompt_sender.try_send(event).is_err() {
                pump_state.recovering.store(true, Ordering::Release);
                if !pump_state.overflowed.swap(true, Ordering::AcqRel) {
                    pump_state.notify.notify_one();
                }
            }
        }
        pump_state.closed.store(true, Ordering::Release);
        pump_state.notify.notify_one();
    });
    (task, state)
}

enum MainTurnAdmission {
    Idle,
    AwaitingNewTurn,
    Active(TurnId),
}

struct PromptEventState {
    session: PacodeSessionId,
    expected: ClientEventContext,
    session_seq: u64,
    session_snapshot_ready: bool,
    snapshot_applied: bool,
    admission: MainTurnAdmission,
    prompt_submitted: bool,
    turn_started_confirmed: bool,
    expected_turn: Option<TurnId>,
    admission_request: Option<Request>,
    fence_attempts: u8,
    prompt_floor_seq: u64,
    prompt_transcript_floor: u64,
}

impl PromptEventState {
    fn new_correlated(
        session: PacodeSessionId,
        context: ClientEventContext,
        session_seq: u64,
        turn_active: bool,
        expected_turn: TurnId,
        admission_request: Option<Request>,
    ) -> Self {
        Self {
            session,
            expected: context,
            session_seq,
            session_snapshot_ready: true,
            snapshot_applied: false,
            admission: if turn_active {
                MainTurnAdmission::AwaitingNewTurn
            } else {
                MainTurnAdmission::Idle
            },
            prompt_submitted: false,
            turn_started_confirmed: false,
            expected_turn: Some(expected_turn),
            admission_request,
            fence_attempts: 0,
            prompt_floor_seq: session_seq,
            prompt_transcript_floor: 0,
        }
    }

    #[cfg(test)]
    fn new(
        session: PacodeSessionId,
        context: ClientEventContext,
        session_seq: u64,
        turn_active: bool,
    ) -> Self {
        Self {
            session,
            expected: context,
            session_seq,
            session_snapshot_ready: true,
            snapshot_applied: false,
            admission: if turn_active {
                MainTurnAdmission::AwaitingNewTurn
            } else {
                MainTurnAdmission::Idle
            },
            prompt_submitted: false,
            turn_started_confirmed: false,
            expected_turn: None,
            admission_request: None,
            fence_attempts: 0,
            prompt_floor_seq: session_seq,
            prompt_transcript_floor: 0,
        }
    }

    #[cfg(test)]
    fn observe(&mut self, client_event: &ClientEvent) -> bool {
        match client_event {
            ClientEvent::Connected { context, .. } => self.accept_connected(context),
            ClientEvent::Snapshot { context, snapshot } => self.accept_snapshot(context, snapshot),
            ClientEvent::ResyncNeeded { context } => self.accept_resync(context),
            ClientEvent::Event {
                context,
                seq,
                event,
            } => self.accept_event(context, *seq, event),
            ClientEvent::Disconnected { .. } | ClientEvent::Reconnecting { .. } => {
                self.session_snapshot_ready = false;
                false
            }
            ClientEvent::AttachFailed {
                context,
                attach,
                reason,
            } => {
                if self.accept_attach_failed(context, attach) {
                    log::error!("session attach failed: {attach:?}: {reason}");
                }
                false
            }
        }
    }

    fn accept_connected(&mut self, context: &ClientEventContext) -> bool {
        let reconnected = context.connection_epoch > self.expected.connection_epoch;
        if !self.accept_context(context) {
            return false;
        }
        if reconnected {
            self.session_snapshot_ready = false;
            self.snapshot_applied = false;
        }
        true
    }

    fn accept_context(&mut self, context: &ClientEventContext) -> bool {
        if context.session.as_ref() != Some(&self.session)
            || context.session_epoch != self.expected.session_epoch
            || context.connection_epoch < self.expected.connection_epoch
        {
            return false;
        }
        self.expected = context.clone();
        true
    }

    fn current_context(&self) -> ClientEventContext {
        self.expected.clone()
    }

    fn accept_attach_failed(&mut self, context: &ClientEventContext, attach: &Attach) -> bool {
        if !matches!(&attach, Attach::Resume { session, .. } if session == &self.session)
            || context
                .session
                .as_ref()
                .is_some_and(|session| session != &self.session)
            || context.connection_epoch < self.expected.connection_epoch
            || (context.connection_epoch == self.expected.connection_epoch
                && context.session_epoch < self.expected.session_epoch)
        {
            return false;
        }
        self.expected.connection_epoch = context.connection_epoch;
        if context.session_epoch != 0 {
            self.expected.session_epoch = context.session_epoch;
        }
        self.session_snapshot_ready = false;
        self.snapshot_applied = false;
        true
    }

    fn accept_snapshot(
        &mut self,
        context: &ClientEventContext,
        snapshot: &SessionSnapshot,
    ) -> bool {
        if snapshot.meta.id != self.session
            || (context == &self.expected
                && self.session_snapshot_ready
                && self.snapshot_applied
                && snapshot.seq < self.session_seq)
            || !self.accept_context(context)
        {
            return false;
        }
        self.session_seq = snapshot.seq;
        self.session_snapshot_ready = true;
        self.snapshot_applied = true;
        if snapshot.turn_active {
            if let Some(expected) = self.expected_turn.as_ref()
                && snapshot.active_turn.as_ref() == Some(expected)
            {
                self.admission = MainTurnAdmission::Active(expected.clone());
                self.turn_started_confirmed = true;
            } else if !matches!(self.admission, MainTurnAdmission::Active(_)) {
                self.admission = MainTurnAdmission::AwaitingNewTurn;
            }
        } else if let Some(expected) = self.expected_turn.as_ref()
            && snapshot.terminal_turn.as_ref() == Some(expected)
        {
            self.admission = MainTurnAdmission::Idle;
            self.turn_started_confirmed = true;
        } else if !self.awaiting_new_turn() {
            self.admission = MainTurnAdmission::Idle;
        }
        true
    }

    fn set_prompt_floor(&mut self, seq: u64, transcript_floor: u64) {
        self.prompt_floor_seq = seq;
        self.prompt_transcript_floor = transcript_floor;
    }

    fn record_snapshot_boundary(&mut self, turn_active: bool) {
        if self.awaiting_new_turn() && !turn_active {
            return;
        }
        self.admission = if turn_active {
            MainTurnAdmission::AwaitingNewTurn
        } else {
            MainTurnAdmission::Idle
        };
    }

    fn accept_resync(&mut self, context: &ClientEventContext) -> bool {
        if !self.accept_context(context) {
            return false;
        }
        self.session_snapshot_ready = false;
        self.snapshot_applied = false;
        true
    }

    fn accept_global_context(&mut self, context: &ClientEventContext) -> bool {
        if context
            .session
            .as_ref()
            .is_some_and(|session| session != &self.session)
        {
            return false;
        }
        if context.connection_epoch == 0 {
            return true;
        }
        if context.connection_epoch < self.expected.connection_epoch {
            return false;
        }
        if context.connection_epoch > self.expected.connection_epoch {
            self.expected.connection_epoch = context.connection_epoch;
            self.session_snapshot_ready = false;
            self.snapshot_applied = false;
        }
        true
    }

    fn accept_event(&mut self, context: &ClientEventContext, seq: u64, event: &Event) -> bool {
        if event.is_global() {
            return self.accept_global_context(context);
        }
        if context != &self.expected {
            return false;
        }
        if !self.session_snapshot_ready || seq <= self.session_seq {
            return false;
        }
        self.session_seq = seq;
        if let Event::TurnStarted { agent, turn } = event
            && agent.is_main()
            && self
                .expected_turn
                .as_ref()
                .is_none_or(|expected| expected == turn)
        {
            self.admission = MainTurnAdmission::Active(turn.clone());
            self.turn_started_confirmed = true;
        }
        true
    }

    fn ends_active_turn(&self, agent: &AgentId, turn: &TurnId) -> bool {
        agent.is_main()
            && matches!(&self.admission, MainTurnAdmission::Active(active) if active == turn)
    }

    fn complete_turn(&mut self, agent: &AgentId, turn: &TurnId) -> bool {
        if !self.ends_active_turn(agent, turn) {
            return false;
        }
        self.admission = MainTurnAdmission::Idle;
        true
    }

    fn prepare_prompt_submission(&mut self, turn_active: bool) {
        self.record_snapshot_boundary(turn_active);
    }

    fn mark_prompt_submitted(&mut self) {
        self.prompt_submitted = true;
    }

    fn awaiting_new_turn(&self) -> bool {
        matches!(self.admission, MainTurnAdmission::AwaitingNewTurn)
    }

    fn needs_admission_fence(&self) -> bool {
        self.prompt_submitted && !self.turn_started_confirmed
    }

    fn take_admission_fence(&mut self) -> Option<Request> {
        if !self.needs_admission_fence()
            || self.fence_attempts >= SESSION_REQUEST_MAX_ATTEMPTS as u8
        {
            return None;
        }
        self.fence_attempts += 1;
        self.admission_request.clone()
    }

    fn idle_snapshot_stop(&self, snapshot: &SessionSnapshot) -> Option<AcpStopReason> {
        if !self.prompt_submitted || !self.turn_started_confirmed || snapshot.turn_active {
            return None;
        }
        if let Some(expected) = self.expected_turn.as_ref()
            && snapshot
                .terminal_turn
                .as_ref()
                .is_some_and(|terminal| terminal != expected)
        {
            return None;
        }
        let main = snapshot.agents.iter().find(|agent| agent.id.is_main());
        let main_status = main.map(|agent| agent.status);
        if self.expected_turn.is_none()
            && main
                .is_some_and(|agent| agent.status == AgentStatus::Failed || agent.error.is_some())
        {
            return None;
        }
        if snapshot.terminal_error.is_some() {
            return None;
        }
        if self.awaiting_new_turn() && main_status != Some(AgentStatus::Stopped) {
            return None;
        }
        Some(if main_status == Some(AgentStatus::Stopped) {
            AcpStopReason::Cancelled
        } else {
            AcpStopReason::EndTurn
        })
    }

    fn snapshot_terminal_error(&self, snapshot: &SessionSnapshot) -> Option<AcpError> {
        if !self.prompt_submitted
            || !self.turn_started_confirmed
            || snapshot.turn_active
            || snapshot.seq <= self.prompt_floor_seq
        {
            return None;
        }
        if let Some(expected) = self.expected_turn.as_ref() {
            if snapshot
                .terminal_turn
                .as_ref()
                .is_some_and(|terminal| terminal != expected)
            {
                return None;
            }
        } else if snapshot.terminal_turn.is_none()
            && snapshot.terminal_error.is_none()
            && let Some(agent) = snapshot.agents.iter().find(|agent| agent.id.is_main())
            && (agent.status == AgentStatus::Failed || agent.error.is_some())
        {
            return Some(AcpError::Client(ClientError::Daemon(
                agent
                    .error
                    .clone()
                    .unwrap_or_else(|| "main turn failed".to_string()),
            )));
        }
        if let Some(error) = snapshot.terminal_error.clone() {
            return Some(AcpError::Client(ClientError::Daemon(error)));
        }
        snapshot.transcript.iter().find_map(|item| {
            if !item.agent.is_main() || item.seq <= self.prompt_transcript_floor {
                return None;
            }
            let pacode_types::TranscriptKind::Notice { level, text } = &item.kind else {
                return None;
            };
            (*level == pacode_types::ToastLevel::Error && text.starts_with("turn failed:")).then(
                || {
                    AcpError::Client(ClientError::Daemon(
                        text.strip_prefix("turn failed: ")
                            .unwrap_or(text)
                            .to_string(),
                    ))
                },
            )
        })
    }
}

#[derive(Clone, Debug, Default)]
struct InitialSnapshotState {
    turn_active: bool,
    pending_permissions: Vec<PermissionRequest>,
    plan: Plan,
    snapshot_seq: u64,
    transcript_floor: u64,
    transcript_items: Vec<(u64, String, bool)>,
    transcript_seen: Vec<u64>,
    transcript_tool_statuses: Vec<(u64, pacode_types::ToolStatus)>,
}

impl From<&SessionSnapshot> for InitialSnapshotState {
    fn from(snapshot: &SessionSnapshot) -> Self {
        Self {
            turn_active: snapshot.turn_active,
            pending_permissions: snapshot.pending_permissions.clone(),
            plan: snapshot.plan.clone(),
            snapshot_seq: snapshot.seq,
            transcript_floor: snapshot
                .transcript
                .iter()
                .filter(|item| item.agent.is_main())
                .map(|item| item.seq)
                .max()
                .unwrap_or(0),
            transcript_seen: snapshot
                .transcript
                .iter()
                .filter(|item| item.agent.is_main())
                .rev()
                .take(INITIAL_TRANSCRIPT_LIMIT)
                .map(|item| item.seq)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            transcript_tool_statuses: snapshot
                .transcript
                .iter()
                .filter(|item| item.agent.is_main())
                .rev()
                .take(INITIAL_TRANSCRIPT_LIMIT)
                .filter_map(|item| match &item.kind {
                    pacode_types::TranscriptKind::ToolCall { status, .. } => {
                        Some((item.seq, *status))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            transcript_items: snapshot
                .transcript
                .iter()
                .filter(|item| item.agent.is_main())
                .rev()
                .take(INITIAL_TRANSCRIPT_LIMIT)
                .filter_map(|item| match &item.kind {
                    pacode_types::TranscriptKind::Assistant { text, complete }
                    | pacode_types::TranscriptKind::Reasoning { text, complete } => Some((
                        item.seq,
                        if *complete {
                            String::new()
                        } else {
                            text.clone()
                        },
                        *complete,
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        }
    }
}

fn expect_session_ok(request_name: &'static str, reply: Reply) -> Result<(), AcpError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error { message } => Err(AcpError::Client(ClientError::Daemon(message))),
        other => Err(AcpError::Client(ClientError::UnexpectedReply {
            request: request_name.to_string(),
            reply: format!("{other:?}"),
        })),
    }
}

fn context_not_older(candidate: &ClientEventContext, stored: &ClientEventContext) -> bool {
    candidate.connection_epoch > stored.connection_epoch
        || (candidate.connection_epoch == stored.connection_epoch
            && candidate.session_epoch >= stored.session_epoch)
}

async fn send_session_ok(
    client: &Client,
    context: ClientEventContext,
    request: Request,
) -> Result<(), AcpError> {
    let request_name = request.name();
    expect_session_ok(
        request_name,
        client.request_session_for(context, request).await?,
    )
}

async fn send_session_ok_with_retry(
    session: &SessionState,
    context: ClientEventContext,
    request: Request,
) -> Result<(), AcpError> {
    let request_name = request.name();
    expect_session_ok(
        request_name,
        session.request_with_retry(context, request).await?,
    )
}

#[cfg(test)]
async fn handle_permissions<'a, I, Reply, ReplyFuture>(
    session: &SessionState,
    permissions: I,
    acp_session_id: &AcpSessionId,
    context: ClientEventContext,
    cx: &ConnectionTo<AcpClient>,
    mut reply: Reply,
) -> Result<(), AcpError>
where
    I: IntoIterator<Item = &'a PermissionRequest> + Send,
    I::IntoIter: Send,
    Reply: FnMut(ClientEventContext, PermissionId, PermissionDecision) -> ReplyFuture + Send,
    ReplyFuture: Future<Output = Result<(), AcpError>> + Send,
{
    let generation = session.cancellation_generation();
    handle_permissions_at_generation(
        session,
        permissions,
        acp_session_id,
        context,
        generation,
        cx,
        move |context, permission, decision, _| reply(context, permission, decision),
    )
    .await
}

async fn handle_permissions_at_generation<'a, I, Reply, ReplyFuture>(
    session: &SessionState,
    permissions: I,
    acp_session_id: &AcpSessionId,
    context: ClientEventContext,
    generation: u64,
    cx: &ConnectionTo<AcpClient>,
    mut reply: Reply,
) -> Result<(), AcpError>
where
    I: IntoIterator<Item = &'a PermissionRequest> + Send,
    I::IntoIter: Send,
    Reply: FnMut(ClientEventContext, PermissionId, PermissionDecision, u64) -> ReplyFuture + Send,
    ReplyFuture: Future<Output = Result<(), AcpError>> + Send,
{
    for permission in permissions {
        if session.cancellation_generation() != generation {
            session.wait_for_interrupt_result(generation).await?;
            return Err(session.cancellation_error());
        }
        if !session
            .handled_permissions
            .lock()
            .await
            .insert(permission.id.clone())
        {
            continue;
        }
        if session.cancellation_generation() != generation {
            session.wait_for_interrupt_result(generation).await?;
            session
                .handled_permissions
                .lock()
                .await
                .remove(&permission.id);
            return Err(session.cancellation_error());
        }
        let request = permission_request_to_acp(acp_session_id, permission);
        let response = tokio::select! {
            _ = session.wait_for_cancellation(generation) => {
                session.wait_for_interrupt_result(generation).await?;
                session
                    .handled_permissions
                    .lock()
                    .await
                    .remove(&permission.id);
                return Err(session.cancellation_error());
            }
            result = cx.send_request(request).block_task() => match result {
                Ok(response) => response,
                Err(error) => {
                    session
                        .handled_permissions
                        .lock()
                        .await
                        .remove(&permission.id);
                    return Err(AcpError::Protocol(error));
                }
            }
        };
        if session.cancellation_generation() != generation {
            session.wait_for_interrupt_result(generation).await?;
            session
                .handled_permissions
                .lock()
                .await
                .remove(&permission.id);
            return Err(session.cancellation_error());
        }
        if let Err(error) = reply(
            context.clone(),
            permission.id.clone(),
            permission_response_to_decision(&response),
            generation,
        )
        .await
        {
            if session.cancellation_generation() != generation {
                session.wait_for_interrupt_result(generation).await?;
            }
            session
                .handled_permissions
                .lock()
                .await
                .remove(&permission.id);
            return Err(error);
        }
    }
    Ok(())
}

struct ResumeResult {
    session: Arc<SessionState>,
    snapshot: Option<SessionSnapshot>,
    config_options: Option<Vec<SessionConfigOption>>,
}

struct HistoryPageCursor {
    before: Option<u64>,
    first: u64,
    last: u64,
}

fn replay_item_bytes(item: &pacode_types::TranscriptItem) -> usize {
    match &item.kind {
        pacode_types::TranscriptKind::User { text }
        | pacode_types::TranscriptKind::Assistant { text, .. }
        | pacode_types::TranscriptKind::Reasoning { text, .. }
        | pacode_types::TranscriptKind::Notice { text, .. } => text.len(),
        pacode_types::TranscriptKind::ToolCall {
            name,
            title,
            preview,
            intent,
            ..
        } => name.len() + title.len() + preview.len() + intent.as_deref().map_or(0, str::len),
        pacode_types::TranscriptKind::BashCommand {
            command, output, ..
        } => command.len() + output.len(),
        pacode_types::TranscriptKind::Permission(permission) => {
            permission.title.len() + permission.detail.len()
        }
        pacode_types::TranscriptKind::Question {
            question, answer, ..
        } => {
            question.question.len()
                + question.header.len()
                + answer
                    .as_ref()
                    .and_then(|answer| answer.free_text.as_deref())
                    .map_or(0, str::len)
        }
    }
}

async fn send_replayed_items<'a, I>(
    items: I,
    acp_session_id: &AcpSessionId,
    cx: &ConnectionTo<AcpClient>,
) -> Result<(), AcpError>
where
    I: IntoIterator<Item = &'a pacode_types::TranscriptItem>,
{
    for item in items {
        for update in transcript_item_to_session_updates(item) {
            cx.send_notification(SessionNotification::new(acp_session_id.clone(), update))
                .map_err(AcpError::Protocol)?;
        }
    }
    Ok(())
}

async fn replay_history_pages(
    client: &Client,
    snapshot: &SessionSnapshot,
    acp_session_id: &AcpSessionId,
    cx: &ConnectionTo<AcpClient>,
) -> Result<(), AcpError> {
    let mut before_seq = None;
    let mut previous_seq = None;
    for item in snapshot
        .transcript
        .iter()
        .filter(|item| item.agent.is_main())
    {
        if previous_seq.is_some_and(|previous| previous >= item.seq) {
            return Err(AcpError::Internal("snapshot history is not ordered".into()));
        }
        if before_seq.is_none() {
            before_seq = Some(item.seq);
        }
        previous_seq = Some(item.seq);
    }
    let mut has_more = snapshot.has_more_history;
    let mut pages = Vec::new();
    let mut first_page_items = None;
    let mut replay_bytes = 0usize;
    let mut bounded = false;
    while has_more {
        if pages.len() >= MAX_HISTORY_PAGES || replay_bytes >= MAX_HISTORY_REPLAY_BYTES {
            bounded = true;
            break;
        }
        let current = client.event_context();
        if current.session.as_ref() != Some(&snapshot.meta.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        let reply = client
            .request_session_for(
                current,
                Request::GetHistory {
                    agent: AgentId::main(),
                    before_seq,
                    limit: HISTORY_PAGE_LIMIT as u32,
                },
            )
            .await?;
        let pacode_types::Reply::History {
            agent,
            items,
            has_more: next,
        } = reply
        else {
            return Err(AcpError::Internal("history reply mismatch".into()));
        };
        if agent != AgentId::main() || items.len() > HISTORY_PAGE_LIMIT {
            return Err(AcpError::Internal("invalid history page".into()));
        }
        if items.is_empty() {
            if next {
                return Err(AcpError::Internal(
                    "history page is empty but claims more history".into(),
                ));
            }
            break;
        }
        let first = items[0].seq;
        let last = items[items.len() - 1].seq;
        if items.windows(2).any(|pair| pair[0].seq >= pair[1].seq)
            || before_seq.is_some_and(|before| first >= before || last >= before)
        {
            return Err(AcpError::Internal("history cursor did not advance".into()));
        }
        let page_bytes: usize = items.iter().map(replay_item_bytes).sum();
        replay_bytes = replay_bytes.saturating_add(page_bytes);
        if pages.is_empty() {
            first_page_items = Some(items);
        }
        pages.push(HistoryPageCursor {
            before: before_seq,
            first,
            last,
        });
        before_seq = Some(first);
        has_more = next;
    }
    if bounded {
        log::warn!(
            "ACP history replay for {} stopped after {} pages / {replay_bytes} bytes",
            snapshot.meta.id,
            pages.len()
        );
    }

    if pages.len() == 1 {
        if let Some(items) = first_page_items.as_deref() {
            send_replayed_items(items.iter(), acp_session_id, cx).await?;
        }
    } else {
        for page in pages.into_iter().rev() {
            let current = client.event_context();
            if current.session.as_ref() != Some(&snapshot.meta.id) {
                return Err(AcpError::Client(ClientError::StaleSession));
            }
            let reply = client
                .request_session_for(
                    current,
                    Request::GetHistory {
                        agent: AgentId::main(),
                        before_seq: page.before,
                        limit: HISTORY_PAGE_LIMIT as u32,
                    },
                )
                .await?;
            let pacode_types::Reply::History {
                agent,
                items,
                has_more: _,
            } = reply
            else {
                return Err(AcpError::Internal("history reply mismatch".into()));
            };
            if agent != AgentId::main()
                || items.len() > HISTORY_PAGE_LIMIT
                || items.first().map(|item| item.seq) != Some(page.first)
                || items.last().map(|item| item.seq) != Some(page.last)
                || items.windows(2).any(|pair| pair[0].seq >= pair[1].seq)
                || items
                    .last()
                    .map(|item| item.seq)
                    .zip(page.before)
                    .is_some_and(|(last, before)| last >= before)
            {
                return Err(AcpError::Internal(
                    "history page changed during replay".into(),
                ));
            }
            send_replayed_items(&items, acp_session_id, cx).await?;
        }
    }

    send_replayed_items(
        snapshot
            .transcript
            .iter()
            .filter(|item| item.agent.is_main()),
        acp_session_id,
        cx,
    )
    .await
}

async fn request_prompt_snapshot_with_retry(
    session: &SessionState,
    context: ClientEventContext,
) -> Result<(ClientEventContext, SessionSnapshot), AcpError> {
    for attempt in 0..SESSION_REQUEST_MAX_ATTEMPTS {
        match session
            .request_with_retry(context.clone(), Request::GetSnapshot)
            .await
        {
            Ok(Reply::Snapshot(snapshot)) => {
                return Ok((session.captured_context().await, snapshot));
            }
            Ok(Reply::Error { message: _ }) if attempt + 1 < SESSION_REQUEST_MAX_ATTEMPTS => {}
            Ok(Reply::Error { message }) => {
                return Err(AcpError::Client(ClientError::Daemon(message)));
            }
            Ok(other) => {
                return Err(AcpError::Client(ClientError::UnexpectedReply {
                    request: "get_snapshot".into(),
                    reply: format!("{other:?}"),
                }));
            }
            Err(error) => return Err(error),
        }
    }
    Err(AcpError::Client(ClientError::Disconnected))
}

struct PromptSnapshot<'a> {
    context: ClientEventContext,
    snapshot: &'a SessionSnapshot,
    acp_session_id: &'a AcpSessionId,
    generation: u64,
}

struct PromptEventInput<'a, Reply> {
    cx: &'a ConnectionTo<AcpClient>,
    acp_session_id: &'a AcpSessionId,
    event_state: &'a mut PromptEventState,
    mapping_state: &'a mut MappingState,
    reply: &'a mut Reply,
    generation: u64,
    allow_completion: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CancellationOutcome {
    Pending,
    Acknowledged,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CancellationStatus {
    generation: u64,
    outcome: CancellationOutcome,
}

async fn handle_prompt_snapshot<Reply, ReplyFuture>(
    session: &SessionState,
    input: PromptSnapshot<'_>,
    cx: &ConnectionTo<AcpClient>,
    event_state: &mut PromptEventState,
    mapping_state: &mut MappingState,
    reply: &mut Reply,
) -> Result<bool, AcpError>
where
    Reply: FnMut(ClientEventContext, PermissionId, PermissionDecision, u64) -> ReplyFuture + Send,
    ReplyFuture: Future<Output = Result<(), AcpError>> + Send,
{
    let PromptSnapshot {
        context,
        snapshot,
        acp_session_id,
        generation,
    } = input;
    if !event_state.accept_snapshot(&context, snapshot) {
        return Ok(false);
    }
    session.remember_context(&context).await;
    for item in &snapshot.transcript {
        let updates = match mapping_state.replay_transcript_item(item) {
            Ok(updates) => updates,
            Err(error) => {
                session.set_fatal_error(error.clone());
                let _ = session.close().await;
                return Err(AcpError::Internal(error));
            }
        };
        for update in updates {
            cx.send_notification(SessionNotification::new(acp_session_id.clone(), update))
                .map_err(AcpError::Protocol)?;
        }
    }
    if !snapshot.plan.is_empty() {
        cx.send_notification(SessionNotification::new(
            acp_session_id.clone(),
            SessionUpdate::Plan(plan_to_acp(&snapshot.plan)),
        ))
        .map_err(AcpError::Protocol)?;
    }
    handle_permissions_at_generation(
        session,
        snapshot.pending_permissions.iter(),
        acp_session_id,
        context,
        generation,
        cx,
        reply,
    )
    .await?;
    let update = {
        let mut config = session.config.lock().await;
        config.meta = snapshot.meta.clone();
        config_options_update(&config, session.config_caps)
    };
    if let Some(update) = update {
        cx.send_notification(SessionNotification::new(acp_session_id.clone(), update))
            .map_err(AcpError::Protocol)?;
    }
    Ok(true)
}

/// An active session connected to the pacode daemon.
pub struct SessionState {
    pub id: PacodeSessionId,
    pub client: Arc<Client>,
    pub events: broadcast::Sender<ClientEvent>,
    event_task: JoinHandle<()>,
    event_pump: Arc<EventPumpState>,
    prompt_events: Mutex<mpsc::Receiver<ClientEvent>>,
    prompt_lock: Mutex<()>,
    context: Mutex<ClientEventContext>,
    initial_snapshot: Mutex<Option<InitialSnapshotState>>,
    mapping_state: Mutex<MappingState>,
    handled_permissions: Mutex<HashSet<PermissionId>>,
    fatal_error: std::sync::Mutex<Option<String>>,
    /// Last known meta + model catalog backing the ACP config options.
    pub config: Mutex<SessionConfigState>,
    /// Config-option features the client advertised at initialize.
    pub config_caps: ConfigOptionCaps,
    submission_lock: Mutex<()>,
    closed: AtomicBool,
    cancellation_active: AtomicBool,
    cancellation_generation: AtomicU64,
    cancellation_tx: watch::Sender<CancellationStatus>,
}

impl Drop for SessionState {
    fn drop(&mut self) {
        self.event_task.abort();
    }
}

impl SessionState {
    fn new(
        id: PacodeSessionId,
        context: ClientEventContext,
        client: Client,
        events: EventReceiver,
        initial_snapshot: InitialSnapshotState,
        config: SessionConfigState,
        config_caps: ConfigOptionCaps,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (prompt_sender, prompt_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (event_task, event_pump) = spawn_event_pump(
            events,
            event_sender.clone(),
            prompt_sender,
            id.clone(),
            context.clone(),
        );
        let mut mapping_state = MappingState::new();
        mapping_state.seed_transcript_with_seen(
            initial_snapshot.transcript_floor,
            &initial_snapshot.transcript_items,
            &initial_snapshot.transcript_seen,
        );
        mapping_state.seed_tool_statuses(&initial_snapshot.transcript_tool_statuses);
        Self {
            id,
            client: Arc::new(client),
            events: event_sender,
            event_task,
            event_pump,
            prompt_events: Mutex::new(prompt_receiver),
            prompt_lock: Mutex::new(()),
            context: Mutex::new(context),
            initial_snapshot: Mutex::new(Some(initial_snapshot)),
            mapping_state: Mutex::new(mapping_state),
            handled_permissions: Mutex::new(HashSet::new()),
            fatal_error: std::sync::Mutex::new(None),
            config: Mutex::new(config),
            config_caps,
            submission_lock: Mutex::new(()),
            closed: AtomicBool::new(false),
            cancellation_active: AtomicBool::new(false),
            cancellation_generation: AtomicU64::new(0),
            cancellation_tx: watch::channel(CancellationStatus {
                generation: 0,
                outcome: CancellationOutcome::Pending,
            })
            .0,
        }
    }

    fn cancellation_generation(&self) -> u64 {
        self.cancellation_generation.load(Ordering::Acquire)
    }

    async fn remember_context(&self, context: &ClientEventContext) {
        if context.session.as_ref() != Some(&self.id) {
            return;
        }
        let mut stored = self.context.lock().await;
        if context_not_older(context, &stored) {
            *stored = context.clone();
        }
    }

    fn set_fatal_error(&self, error: String) {
        *self
            .fatal_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
    }

    fn fatal_error(&self) -> Option<String> {
        self.fatal_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn cancellation_error(&self) -> AcpError {
        if self.closed.load(Ordering::Acquire) {
            AcpError::Internal("session is closed".into())
        } else {
            AcpError::Internal("session cancellation requested".into())
        }
    }

    fn signal_cancellation(&self) -> u64 {
        let generation = self.cancellation_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.cancellation_active.store(true, Ordering::Release);
        self.cancellation_tx.send_replace(CancellationStatus {
            generation,
            outcome: CancellationOutcome::Pending,
        });
        generation
    }

    fn finish_cancellation(&self, generation: u64, error: Option<&AcpError>) {
        self.cancellation_tx.send_replace(CancellationStatus {
            generation,
            outcome: match error {
                Some(error) => CancellationOutcome::Failed(error.to_string()),
                None => CancellationOutcome::Acknowledged,
            },
        });
        self.cancellation_active.store(false, Ordering::Release);
    }

    async fn wait_for_cancellation(&self, generation: u64) {
        let mut receiver = self.cancellation_tx.subscribe();
        let mut armed = false;
        loop {
            let status = receiver.borrow().clone();
            if status.generation > generation {
                return;
            }
            if status.generation == generation
                && status.outcome == CancellationOutcome::Pending
                && self.cancellation_active.load(Ordering::Acquire)
            {
                armed = true;
            }
            if armed && status.outcome != CancellationOutcome::Pending {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    async fn wait_for_interrupt_result(&self, generation: u64) -> Result<(), AcpError> {
        let mut receiver = self.cancellation_tx.subscribe();
        let mut target = generation.max(receiver.borrow().generation);
        loop {
            let status = receiver.borrow().clone();
            target = target.max(status.generation);
            if status.generation == target {
                match status.outcome {
                    CancellationOutcome::Pending => {}
                    CancellationOutcome::Acknowledged => return Ok(()),
                    CancellationOutcome::Failed(message) => {
                        return Err(AcpError::Internal(format!(
                            "failed to deliver session cancellation: {message}"
                        )));
                    }
                }
            }
            if receiver.changed().await.is_err() {
                return Err(AcpError::Client(ClientError::Disconnected));
            }
        }
    }

    async fn stop_after_interrupt(&self, generation: u64) -> Result<AcpStopReason, AcpError> {
        self.wait_for_interrupt_result(generation).await?;
        Ok(AcpStopReason::Cancelled)
    }

    async fn captured_context(&self) -> ClientEventContext {
        let current = self.client.event_context();
        let mut stored = self.context.lock().await;
        if current.session.as_ref() != Some(&self.id) {
            return current;
        }
        if !context_not_older(&current, &stored) {
            return current;
        }
        if current != *stored {
            *stored = current.clone();
        }
        current
    }

    fn is_usable(&self) -> bool {
        if self.closed.load(Ordering::Acquire)
            || self.event_pump.attach_failed.load(Ordering::Acquire)
            || self.event_pump.closed.load(Ordering::Acquire)
        {
            return false;
        }
        let context = self.client.event_context();
        context.session.as_ref() == Some(&self.id)
            || (!self.client.is_connected() && context.session.is_none())
    }

    async fn wait_for_reconnected_context(
        &self,
        previous: &ClientEventContext,
        events: &mut broadcast::Receiver<ClientEvent>,
    ) -> Result<ClientEventContext, ClientError> {
        let deadline = tokio::time::Instant::now() + RECONNECT_CONTEXT_TIMEOUT;
        loop {
            let current = self.client.event_context();
            if current.session.is_some() && current.session.as_ref() != Some(&self.id) {
                return Err(ClientError::StaleSession);
            }
            if current.session.as_ref() == Some(&self.id)
                && (current.connection_epoch > previous.connection_epoch
                    || (current.connection_epoch == previous.connection_epoch
                        && current.session_epoch > previous.session_epoch))
            {
                return Ok(current);
            }
            let connected = match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Ok(ClientEvent::Connected { context, .. })) => context,
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return Err(ClientError::Disconnected),
            };
            if connected.session.as_ref() == Some(&self.id)
                && (connected.connection_epoch > previous.connection_epoch
                    || (connected.connection_epoch == previous.connection_epoch
                        && connected.session_epoch > previous.session_epoch))
            {
                return Ok(connected);
            }
        }
    }

    async fn request_with_retry(
        &self,
        mut context: ClientEventContext,
        request: Request,
    ) -> Result<Reply, AcpError> {
        let mut events = self.events.subscribe();
        for attempt in 0..SESSION_REQUEST_MAX_ATTEMPTS {
            match self
                .client
                .request_session_for(context.clone(), request.clone())
                .await
            {
                Ok(reply) => return Ok(reply),
                Err(
                    ClientError::Disconnected | ClientError::StaleSession | ClientError::Timeout(_),
                ) if attempt + 1 < SESSION_REQUEST_MAX_ATTEMPTS => {
                    context = self
                        .wait_for_reconnected_context(&context, &mut events)
                        .await?;
                    self.remember_context(&context).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(AcpError::Client(ClientError::Disconnected))
    }

    async fn reissue_admission_fence(
        &self,
        request: Request,
        generation: u64,
    ) -> Result<(), AcpError> {
        let _guard = self.submission_lock.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AcpError::Internal("session is closed".into()));
        }
        if self.cancellation_generation() != generation {
            return Err(self.cancellation_error());
        }
        let context = self.captured_context().await;
        if context.session.as_ref() != Some(&self.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        send_session_ok_with_retry(self, context, request).await
    }

    #[cfg(test)]
    async fn reply_permission(
        &self,
        context: ClientEventContext,
        permission: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), AcpError> {
        self.reply_permission_at_generation(
            context,
            permission,
            decision,
            self.cancellation_generation(),
        )
        .await
    }

    async fn reply_permission_at_generation(
        &self,
        context: ClientEventContext,
        permission: PermissionId,
        decision: PermissionDecision,
        generation: u64,
    ) -> Result<(), AcpError> {
        let _guard = self.submission_lock.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AcpError::Internal("session is closed".into()));
        }
        if self.cancellation_generation() != generation {
            return Err(self.cancellation_error());
        }
        let request = Request::PermissionReply {
            permission,
            decision,
            reason: None,
        };
        let request_name = request.name();
        match self.request_with_retry(context, request).await? {
            Reply::Ok => Ok(()),
            Reply::Error { message } if message == "unknown permission request" => Ok(()),
            Reply::Error { message } => Err(AcpError::Client(ClientError::Daemon(message))),
            other => Err(AcpError::Client(ClientError::UnexpectedReply {
                request: request_name.to_string(),
                reply: format!("{other:?}"),
            })),
        }
    }

    async fn process_prompt_event<Reply, ReplyFuture>(
        &self,
        client_event: ClientEvent,
        input: PromptEventInput<'_, Reply>,
    ) -> Result<Option<AcpStopReason>, AcpError>
    where
        Reply:
            FnMut(ClientEventContext, PermissionId, PermissionDecision, u64) -> ReplyFuture + Send,
        ReplyFuture: Future<Output = Result<(), AcpError>> + Send,
    {
        let PromptEventInput {
            cx,
            acp_session_id,
            event_state,
            mapping_state,
            reply,
            generation,
            allow_completion,
        } = input;
        if self.cancellation_generation() != generation {
            return Err(self.cancellation_error());
        }
        match client_event {
            ClientEvent::Event {
                context,
                seq,
                event,
            } => {
                if !event_state.accept_event(&context, seq, &event) {
                    return Ok(None);
                }
                if let Event::PermissionRequested(permission) = &event {
                    handle_permissions_at_generation(
                        self,
                        std::iter::once(permission),
                        acp_session_id,
                        context,
                        generation,
                        cx,
                        reply,
                    )
                    .await?;
                    return Ok(None);
                }
                if let Event::TurnEnded {
                    agent, turn, stop, ..
                } = &event
                {
                    if allow_completion && agent.is_main() && event_state.awaiting_new_turn() {
                        let mut recovery_context = context.clone();
                        for _ in 0..SESSION_REQUEST_MAX_ATTEMPTS {
                            let (snapshot_context, snapshot) =
                                request_prompt_snapshot_with_retry(self, recovery_context.clone())
                                    .await?;
                            let applied = handle_prompt_snapshot(
                                self,
                                PromptSnapshot {
                                    context: snapshot_context.clone(),
                                    snapshot: &snapshot,
                                    acp_session_id,
                                    generation,
                                },
                                cx,
                                event_state,
                                mapping_state,
                                reply,
                            )
                            .await?;
                            if !applied {
                                recovery_context = snapshot_context;
                                continue;
                            }
                            event_state.record_snapshot_boundary(snapshot.turn_active);
                            if event_state.needs_admission_fence()
                                && let Some(request) = event_state.take_admission_fence()
                            {
                                self.reissue_admission_fence(request, generation).await?;
                                return Ok(None);
                            }
                            if let Some(error) = event_state.snapshot_terminal_error(&snapshot) {
                                mapping_state.finish_turn();
                                return Err(error);
                            }
                            if let Some(stop_reason) = event_state.idle_snapshot_stop(&snapshot) {
                                mapping_state.finish_turn();
                                return Ok(Some(stop_reason));
                            }
                            return Ok(None);
                        }
                        return Err(AcpError::Client(ClientError::Daemon(
                            "prompt snapshot did not advance the turn boundary".into(),
                        )));
                    }
                    if event_state.complete_turn(agent, turn) {
                        mapping_state.finish_turn();
                        if allow_completion {
                            return turn_stop_to_stop_reason(stop)
                                .map(Some)
                                .map_err(|message| AcpError::Client(ClientError::Daemon(message)));
                        }
                    }
                }
                let config_update = self.config_update(&event).await;
                let updates = match &event {
                    Event::TurnEnded { .. } => Vec::new(),
                    _ => event_to_session_updates(&event, mapping_state),
                };
                for update in updates.into_iter().chain(config_update) {
                    cx.send_notification(SessionNotification::new(acp_session_id.clone(), update))
                        .map_err(AcpError::Protocol)?;
                }
            }
            ClientEvent::Snapshot { context, snapshot } => {
                let applied = handle_prompt_snapshot(
                    self,
                    PromptSnapshot {
                        context,
                        snapshot: &snapshot,
                        acp_session_id,
                        generation,
                    },
                    cx,
                    event_state,
                    mapping_state,
                    reply,
                )
                .await?;
                if applied && allow_completion {
                    if event_state.needs_admission_fence()
                        && let Some(request) = event_state.take_admission_fence()
                    {
                        self.reissue_admission_fence(request, generation).await?;
                        return Ok(None);
                    }
                    if let Some(error) = event_state.snapshot_terminal_error(&snapshot) {
                        mapping_state.finish_turn();
                        return Err(error);
                    }
                    if let Some(stop_reason) = event_state.idle_snapshot_stop(&snapshot) {
                        mapping_state.finish_turn();
                        return Ok(Some(stop_reason));
                    }
                }
            }
            ClientEvent::ResyncNeeded { context } => {
                if !event_state.accept_resync(&context) || !self.client.is_connected() {
                    self.event_pump.finish_recovery();
                    return Ok(None);
                }
                let (snapshot_context, snapshot) =
                    match request_prompt_snapshot_with_retry(self, context.clone()).await {
                        Ok(pair) => pair,
                        Err(AcpError::Client(ClientError::Disconnected)) => {
                            self.event_pump.finish_recovery();
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    };
                let applied = handle_prompt_snapshot(
                    self,
                    PromptSnapshot {
                        context: snapshot_context,
                        snapshot: &snapshot,
                        acp_session_id,
                        generation,
                    },
                    cx,
                    event_state,
                    mapping_state,
                    reply,
                )
                .await?;
                if !applied {
                    self.event_pump.finish_recovery();
                    return Ok(None);
                }
                if allow_completion {
                    if event_state.needs_admission_fence()
                        && let Some(request) = event_state.take_admission_fence()
                    {
                        let result = self.reissue_admission_fence(request, generation).await;
                        self.event_pump.finish_recovery();
                        result?;
                        return Ok(None);
                    }
                    if let Some(error) = event_state.snapshot_terminal_error(&snapshot) {
                        mapping_state.finish_turn();
                        self.event_pump.finish_recovery();
                        return Err(error);
                    }
                    if let Some(stop_reason) = event_state.idle_snapshot_stop(&snapshot) {
                        mapping_state.finish_turn();
                        self.event_pump.finish_recovery();
                        return Ok(Some(stop_reason));
                    }
                }
                self.event_pump.finish_recovery();
            }
            ClientEvent::Connected { context, .. } => {
                if event_state.accept_connected(&context) {
                    self.remember_context(&context).await;
                }
            }
            ClientEvent::AttachFailed {
                context,
                attach,
                reason,
            } => {
                if !event_state.accept_attach_failed(&context, &attach) {
                    return Ok(None);
                }
                log::error!("session attach failed: {attach:?}: {reason}");
                return Err(AcpError::Client(ClientError::Daemon(reason)));
            }
            ClientEvent::Disconnected { reason, .. } => {
                event_state.session_snapshot_ready = false;
                event_state.snapshot_applied = false;
                log::warn!("daemon disconnected: {reason}");
            }
            ClientEvent::Reconnecting { attempt, .. } => {
                event_state.session_snapshot_ready = false;
                event_state.snapshot_applied = false;
                log::info!("reconnecting to daemon, attempt {attempt}");
            }
        }
        Ok(None)
    }

    /// Whether a prompt turn is in flight. `run_prompt` holds the event-stream
    /// lock for the whole turn, so the lock itself is the marker; there is no
    /// separate flag that could drift out of sync with it.
    pub fn is_busy(&self) -> bool {
        self.prompt_lock.try_lock().is_err()
    }

    async fn deliver_interrupt_all(&self) -> Result<(), AcpError> {
        let context = self.captured_context().await;
        if context.session.as_ref() != Some(&self.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        expect_session_ok(
            Request::InterruptAll.name(),
            self.request_with_retry(context, Request::InterruptAll)
                .await?,
        )
    }

    async fn wait_for_connection(
        &self,
        events: &mut broadcast::Receiver<ClientEvent>,
    ) -> Result<(), ClientError> {
        let deadline = tokio::time::Instant::now() + RECONNECT_CONTEXT_TIMEOUT;
        loop {
            if self.client.is_connected() {
                return Ok(());
            }
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Ok(ClientEvent::Connected { .. })) => return Ok(()),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return Err(ClientError::Disconnected),
            }
        }
    }

    /// Sends an interrupt request to the daemon.
    pub async fn interrupt(&self) -> Result<(), AcpError> {
        let _guard = self.submission_lock.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AcpError::Internal("session is closed".into()));
        }
        let generation = self.signal_cancellation();
        match self.deliver_interrupt_all().await {
            Ok(()) => {
                self.finish_cancellation(generation, None);
                Ok(())
            }
            Err(error) => {
                self.finish_cancellation(generation, Some(&error));
                Err(error)
            }
        }
    }

    async fn close(&self) -> Result<(), AcpError> {
        let _guard = self.submission_lock.lock().await;
        let generation = if self.closed.swap(true, Ordering::AcqRel) {
            self.cancellation_generation()
        } else {
            self.signal_cancellation()
        };
        let mut events = self.events.subscribe();
        let mut last_error = None;
        for attempt in 0..SESSION_REQUEST_MAX_ATTEMPTS {
            match self.client.close_session().await {
                Ok(reply) => match expect_session_ok(Request::CloseSession.name(), reply) {
                    Ok(()) => {
                        self.finish_cancellation(generation, None);
                        self.handled_permissions.lock().await.clear();
                        return Ok(());
                    }
                    Err(error) => {
                        self.finish_cancellation(generation, Some(&error));
                        return Err(error);
                    }
                },
                Err(error)
                    if matches!(
                        error,
                        ClientError::Disconnected
                            | ClientError::StaleSession
                            | ClientError::Timeout(_)
                    ) && attempt + 1 < SESSION_REQUEST_MAX_ATTEMPTS =>
                {
                    last_error = Some(error);
                    if let Err(error) = self.wait_for_connection(&mut events).await {
                        let error = AcpError::Client(error);
                        self.finish_cancellation(generation, Some(&error));
                        return Err(error);
                    }
                }
                Err(error) => {
                    let error = AcpError::Client(error);
                    self.finish_cancellation(generation, Some(&error));
                    return Err(error);
                }
            }
        }
        let error = last_error
            .map(AcpError::Client)
            .unwrap_or_else(|| AcpError::Client(ClientError::Disconnected));
        self.finish_cancellation(generation, Some(&error));
        Err(error)
    }

    /// Runs a prompt turn, streaming updates to the client and handling permissions.
    pub async fn run_prompt(
        &self,
        prompt: &PromptRequest,
        cx: &ConnectionTo<AcpClient>,
    ) -> Result<AcpStopReason, AcpError> {
        let (text, images) = prompt_content_to_parts(&prompt.prompt);
        let acp_session_id = prompt.session_id.clone();
        let message_id = format!("acp-{}", ClientId::generate());
        let admission = Request::UserMessage {
            text,
            images,
            message_id: Some(message_id.clone()),
        };
        let expected_turn = TurnId::new(format!("trn_{message_id}"));

        let _prompt_guard = self.prompt_lock.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Ok(AcpStopReason::Cancelled);
        }
        let generation = self.cancellation_generation();
        let event_pump = Arc::clone(&self.event_pump);
        let mut events = self.prompt_events.lock().await;
        while events.try_recv().is_ok() {}
        event_pump.overflowed.store(false, Ordering::Release);
        event_pump.finish_recovery();
        event_pump.prompt_active.store(true, Ordering::Release);
        let _activation = PromptActivationGuard {
            state: Arc::clone(&event_pump),
        };
        let context = self.client.event_context();
        self.remember_context(&context).await;
        let initial = self
            .initial_snapshot
            .lock()
            .await
            .clone()
            .unwrap_or_default();
        let mut event_state = PromptEventState::new_correlated(
            self.id.clone(),
            context.clone(),
            initial.snapshot_seq,
            initial.turn_active,
            expected_turn,
            Some(admission.clone()),
        );

        let mut mapping_state = self.mapping_state.lock().await;
        let mut reply_permission = |context: ClientEventContext,
                                    permission: PermissionId,
                                    decision: PermissionDecision,
                                    generation: u64| {
            self.reply_permission_at_generation(context, permission, decision, generation)
        };

        if !initial.pending_permissions.is_empty() {
            log::debug!(
                "preflight will reconcile {} initial pending permissions",
                initial.pending_permissions.len()
            );
        }
        let mut preflight = None;
        for _ in 0..SESSION_REQUEST_MAX_ATTEMPTS {
            let (snapshot_context, snapshot) =
                match request_prompt_snapshot_with_retry(self, context.clone()).await {
                    Ok(pair) => pair,
                    Err(_error) if self.cancellation_generation() != generation => {
                        return self.stop_after_interrupt(generation).await;
                    }
                    Err(error) => return Err(error),
                };
            match handle_prompt_snapshot(
                self,
                PromptSnapshot {
                    context: snapshot_context,
                    snapshot: &snapshot,
                    acp_session_id: &acp_session_id,
                    generation,
                },
                cx,
                &mut event_state,
                &mut mapping_state,
                &mut reply_permission,
            )
            .await
            {
                Ok(true) => {
                    preflight = Some(snapshot);
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    if let Some(fatal) = self.fatal_error() {
                        return Err(AcpError::Internal(fatal));
                    }
                    if self.cancellation_generation() != generation {
                        return self.stop_after_interrupt(generation).await;
                    }
                    return Err(error);
                }
            }
        }
        let Some(preflight_snapshot) = preflight else {
            return Err(AcpError::Client(ClientError::StaleSession));
        };
        *self.initial_snapshot.lock().await = Some(InitialSnapshotState::from(&preflight_snapshot));
        event_state.set_prompt_floor(
            preflight_snapshot.seq,
            preflight_snapshot
                .transcript
                .iter()
                .filter(|item| item.agent.is_main())
                .map(|item| item.seq)
                .max()
                .unwrap_or(0),
        );
        event_state.prepare_prompt_submission(preflight_snapshot.turn_active);
        let submission = {
            let _submission_guard = self.submission_lock.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Ok(AcpStopReason::Cancelled);
            }
            if self.cancellation_generation() != generation {
                None
            } else {
                Some(send_session_ok_with_retry(self, self.client.event_context(), admission).await)
            }
        };
        match submission {
            Some(Ok(())) => {}
            Some(Err(error)) => return Err(error),
            None => return self.stop_after_interrupt(generation).await,
        }
        event_state.mark_prompt_submitted();

        loop {
            if event_pump.closed.load(Ordering::Acquire) {
                if self.cancellation_generation() != generation {
                    return self.stop_after_interrupt(generation).await;
                }
                return Err(AcpError::Client(ClientError::Disconnected));
            }
            let client_event = if event_pump.overflowed.swap(false, Ordering::AcqRel) {
                while events.try_recv().is_ok() {}
                let current = self.client.event_context();
                let context = if current.session.as_ref() == Some(&self.id) {
                    current
                } else {
                    event_state.current_context()
                };
                ClientEvent::ResyncNeeded { context }
            } else {
                tokio::select! {
                    result = events.recv() => match result {
                        Some(event) => event,
                        None => {
                            if self.cancellation_generation() != generation {
                                return self.stop_after_interrupt(generation).await;
                            }
                            return Err(AcpError::Client(ClientError::Disconnected));
                        }
                    },
                    _ = self.wait_for_cancellation(generation) => {
                        return self.stop_after_interrupt(generation).await;
                    }
                    _ = event_pump.notify.notified() => {
                        if event_pump.closed.load(Ordering::Acquire) {
                            if self.cancellation_generation() != generation {
                                return self.stop_after_interrupt(generation).await;
                            }
                            return Err(AcpError::Client(ClientError::Disconnected));
                        }
                        continue;
                    }
                }
            };
            let processed = self
                .process_prompt_event(
                    client_event,
                    PromptEventInput {
                        cx,
                        acp_session_id: &acp_session_id,
                        event_state: &mut event_state,
                        mapping_state: &mut mapping_state,
                        reply: &mut reply_permission,
                        generation,
                        allow_completion: true,
                    },
                )
                .await;
            let stop_reason = match processed {
                Ok(stop_reason) => stop_reason,
                Err(_) if self.fatal_error().is_some() => {
                    return Err(AcpError::Internal(self.fatal_error().unwrap_or_default()));
                }
                Err(_) if self.cancellation_generation() != generation => {
                    return self.stop_after_interrupt(generation).await;
                }
                Err(error) => return Err(error),
            };
            if let Some(stop_reason) = stop_reason {
                return Ok(stop_reason);
            }
        }
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
    closed_sessions: Mutex<HashSet<PacodeSessionId>>,
    order: Mutex<VecDeque<PacodeSessionId>>,
    resume_lock: Mutex<()>,
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
            closed_sessions: Mutex::new(HashSet::new()),
            order: Mutex::new(VecDeque::new()),
            resume_lock: Mutex::new(()),
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
        snapshot: &SessionSnapshot,
    ) -> (Arc<SessionState>, Option<Vec<SessionConfigOption>>) {
        let caps = self.config_caps();
        let context = client.event_context();
        let models = self.fetch_models(&client).await;
        let options = caps
            .select
            .then(|| session_config_options(&snapshot.meta, &models, caps));
        let session = Arc::new(SessionState::new(
            id,
            context,
            client,
            events,
            InitialSnapshotState::from(snapshot),
            SessionConfigState {
                meta: snapshot.meta.clone(),
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
    async fn insert(&self, id: PacodeSessionId, session: Arc<SessionState>) -> Arc<SessionState> {
        self.closed_sessions.lock().await.remove(&id);
        let mut order = self.order.lock().await;
        let mut sessions = self.sessions.write().await;

        if let Some(existing) = sessions.get(&id) {
            if existing.is_usable() {
                let existing = Arc::clone(existing);
                touch_order(&mut order, &id);
                return existing;
            }
            sessions.insert(id.clone(), Arc::clone(&session));
            touch_order(&mut order, &id);
            return session;
        }

        if sessions.len() >= self.max_sessions {
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
        sessions.insert(id, Arc::clone(&session));
        session
    }

    /// Marks `id` as most recently used.
    async fn touch(&self, id: &PacodeSessionId) {
        let mut order = self.order.lock().await;
        touch_order(&mut order, id);
    }

    /// Gets an existing session or resumes it from the daemon if known.
    async fn get_or_resume_with_snapshot(
        &self,
        id: &PacodeSessionId,
    ) -> Result<ResumeResult, AcpError> {
        if self.closed_sessions.lock().await.contains(id) {
            return Err(AcpError::SessionNotFound(id.to_string()));
        }
        let _resume_guard = self.resume_lock.lock().await;
        if self.closed_sessions.lock().await.contains(id) {
            return Err(AcpError::SessionNotFound(id.to_string()));
        }
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(id)
                && session.is_usable()
            {
                let session = Arc::clone(session);
                drop(sessions);
                self.touch(id).await;
                return Ok(ResumeResult {
                    session,
                    snapshot: None,
                    config_options: None,
                });
            }
        }

        let (client, events) = Client::connect(self.client_opts.clone()).await?;
        let snapshot = client
            .attach(Attach::Resume {
                session: id.clone(),
            })
            .await?;
        let (session, options) = self
            .build_state(id.clone(), client, events, &snapshot)
            .await;
        let session = self.insert(id.clone(), session).await;
        Ok(ResumeResult {
            session,
            snapshot: Some(snapshot),
            config_options: options,
        })
    }

    pub async fn get_or_resume(&self, id: &PacodeSessionId) -> Result<Arc<SessionState>, AcpError> {
        Ok(self.get_or_resume_with_snapshot(id).await?.session)
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
            .build_state(session_id.clone(), client, events, &snapshot)
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
        let ResumeResult {
            session,
            snapshot,
            config_options,
        } = self.get_or_resume_with_snapshot(&pacode_id).await?;
        let snapshot = if let Some(snapshot) = snapshot {
            snapshot
        } else {
            let context = session.captured_context().await;
            request_prompt_snapshot_with_retry(&session, context)
                .await?
                .1
        };
        *session.initial_snapshot.lock().await = Some(InitialSnapshotState::from(&snapshot));
        if req.cwd != snapshot.meta.cwd {
            return Err(AcpError::Internal(
                "session cwd does not match request".into(),
            ));
        }
        let config_options = {
            let config = session.config.lock().await;
            config_options.or_else(|| {
                session.config_caps.select.then(|| {
                    session_config_options(&config.meta, &config.models, session.config_caps)
                })
            })
        };

        replay_history_pages(session.client.as_ref(), &snapshot, &req.session_id, cx).await?;

        if !snapshot.plan.is_empty() {
            cx.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::Plan(plan_to_acp(&snapshot.plan)),
            ))
            .map_err(AcpError::Protocol)?;
        }

        let context = session.captured_context().await;
        if context.session.as_ref() != Some(&session.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        let generation = session.cancellation_generation();
        let mut reply_permission = |context: ClientEventContext,
                                    permission: PermissionId,
                                    decision: PermissionDecision,
                                    generation: u64| {
            session.reply_permission_at_generation(context, permission, decision, generation)
        };
        handle_permissions_at_generation(
            &session,
            snapshot.pending_permissions.iter(),
            &req.session_id,
            context,
            generation,
            cx,
            &mut reply_permission,
        )
        .await?;

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
        let ResumeResult {
            session,
            snapshot: mut fresh_snapshot,
            config_options: fresh_options,
        } = self.get_or_resume_with_snapshot(&pacode_id).await?;
        if fresh_snapshot.is_none() {
            let context = session.captured_context().await;
            let (_, snapshot) = request_prompt_snapshot_with_retry(&session, context).await?;
            *session.initial_snapshot.lock().await = Some(InitialSnapshotState::from(&snapshot));
            fresh_snapshot = Some(snapshot);
        }
        let (meta, plan, pending_permissions, config_options) =
            if let Some(snapshot) = fresh_snapshot {
                let config = session.config.lock().await;
                let options = fresh_options.or_else(|| {
                    session.config_caps.select.then(|| {
                        session_config_options(&config.meta, &config.models, session.config_caps)
                    })
                });
                (
                    snapshot.meta.clone(),
                    snapshot.plan.clone(),
                    snapshot.pending_permissions.clone(),
                    options,
                )
            } else {
                let initial = session
                    .initial_snapshot
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_default();
                let config = session.config.lock().await;
                let options = session.config_caps.select.then(|| {
                    session_config_options(&config.meta, &config.models, session.config_caps)
                });
                (
                    config.meta.clone(),
                    initial.plan,
                    initial.pending_permissions,
                    options,
                )
            };
        if req.cwd != meta.cwd {
            return Err(AcpError::Internal(
                "session cwd does not match request".into(),
            ));
        }

        if !plan.is_empty() {
            cx.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::Plan(plan_to_acp(&plan)),
            ))
            .map_err(AcpError::Protocol)?;
        }

        let context = session.captured_context().await;
        if context.session.as_ref() != Some(&session.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        let generation = session.cancellation_generation();
        let mut reply_permission = |context: ClientEventContext,
                                    permission: PermissionId,
                                    decision: PermissionDecision,
                                    generation: u64| {
            session.reply_permission_at_generation(context, permission, decision, generation)
        };
        handle_permissions_at_generation(
            &session,
            pending_permissions.iter(),
            &req.session_id,
            context,
            generation,
            cx,
            &mut reply_permission,
        )
        .await?;

        Ok(ResumeSessionResponse::new()
            .modes(modes_state(meta.mode))
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
        let context = session.captured_context().await;
        if context.session.as_ref() != Some(&session.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        send_session_ok(session.client.as_ref(), context, Request::SetMode(mode)).await?;

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
        let context = session.captured_context().await;
        if context.session.as_ref() != Some(&session.id) {
            return Err(AcpError::Client(ClientError::StaleSession));
        }
        send_session_ok(session.client.as_ref(), context, change.to_request()).await?;

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
        let _resume_guard = self.resume_lock.lock().await;
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(&pacode_id).cloned()
        };
        let had_session = session.is_some();
        if let Some(session) = session.as_ref() {
            if session.is_usable() {
                session.close().await?;
            } else {
                {
                    let mut sessions = self.sessions.write().await;
                    sessions.remove(&pacode_id);
                }
                self.order.lock().await.retain(|id| id != &pacode_id);
                let (client, _events) = Client::connect(self.client_opts.clone()).await?;
                client
                    .attach(Attach::Resume {
                        session: pacode_id.clone(),
                    })
                    .await?;
                expect_session_ok(Request::CloseSession.name(), client.close_session().await?)?;
            }
        } else {
            let (client, _events) = Client::connect(self.client_opts.clone()).await?;
            client
                .attach(Attach::Resume {
                    session: pacode_id.clone(),
                })
                .await?;
            expect_session_ok(Request::CloseSession.name(), client.close_session().await?)?;
        }
        let mut closed_sessions = self.closed_sessions.lock().await;
        let mut order = self.order.lock().await;
        let mut sessions = self.sessions.write().await;
        if had_session {
            sessions.remove(&pacode_id);
            order.retain(|id| id != &pacode_id);
            closed_sessions.insert(pacode_id);
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
