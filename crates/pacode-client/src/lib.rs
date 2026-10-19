//! Daemon connection for frontends (TUI, `pacode run`, `pacode sessions`).
//!
//! - [`Client::connect`]: connect to the socket; when it is missing/refused and
//!   `spawn_daemon` is set, run `<exe> serve --detach` and poll the socket (10 ms,
//!   up to 3 s). Sends `Hello`; a protocol mismatch is an error, a daemon version
//!   mismatch is reported in `Connected` so the caller may ask for a restart.
//! - [`Client::attach`]: bind to a session; the attach is remembered as
//!   `Attach::Resume{session}` for reconnects.
//! - Reconnect: on EOF/error the reader task emits `Disconnected`, retries with
//!   backoff 0.5 s → 8 s (`Reconnecting{attempt}`), re-sends `Hello` + `Attach`, then
//!   emits `Connected` and `Snapshot` (from the `Attached` reply). Pending requests
//!   fail with `ClientError::Disconnected`.
//! - Wire: NDJSON `Envelope` out, `ServerMessage` in; replies matched by id
//!   (`tokio::sync::oneshot`). Client events use a bounded queue: session deltas
//!   are coalesced into one resync marker, while control events are retained or
//!   terminate the stream explicitly.

pub mod connection;
pub mod daemon_ctl;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use pacode_config::Paths;
use pacode_types::{Attach, Event, Reply, Request, SessionId, SessionSnapshot};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub use connection::Client;
pub use daemon_ctl::{DaemonStatus, daemon_status, spawn_daemon, stop_daemon};

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub paths: Paths,
    /// Overrides `paths.socket_path()`.
    pub socket: Option<PathBuf>,
    pub app_version: String,
    pub spawn_daemon: bool,
    /// Executable used to spawn the daemon (default: `std::env::current_exe()`).
    pub exe: Option<PathBuf>,
    /// Request timeout.
    pub request_timeout: std::time::Duration,
}

impl ClientOptions {
    pub fn new(paths: Paths, app_version: impl Into<String>) -> Self {
        Self {
            paths,
            socket: None,
            app_version: app_version.into(),
            spawn_daemon: true,
            exe: None,
            request_timeout: std::time::Duration::from_secs(30),
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.paths.socket_path())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEventContext {
    pub connection_epoch: u64,
    pub session_epoch: u64,
    pub session: Option<SessionId>,
}

impl ClientEventContext {
    pub fn unscoped() -> Self {
        Self {
            connection_epoch: 0,
            session_epoch: 0,
            session: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AttachAttempt {
    pub(crate) request_id: u64,
    pub(crate) idempotency_key: String,
    pub(crate) target: Attach,
    pub(crate) session_epoch: u64,
    pub(crate) connection_epoch: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapshotObservation {
    pub(crate) monotonic: bool,
}

pub(crate) struct LifecycleState {
    connection_epoch: Arc<AtomicU64>,
    session_epoch: Arc<AtomicU64>,
    desired_attach: Arc<Mutex<Option<AttachAttempt>>>,
    attached_session: Arc<Mutex<Option<SessionId>>>,
    snapshot_seq: Arc<Mutex<u64>>,
    last_session_seq: Arc<Mutex<u64>>,
    pub(crate) transition_lock: Arc<Mutex<()>>,
}

impl LifecycleState {
    pub(crate) fn new(connection_epoch: u64) -> Self {
        Self {
            connection_epoch: Arc::new(AtomicU64::new(connection_epoch)),
            session_epoch: Arc::new(AtomicU64::new(0)),
            desired_attach: Arc::new(Mutex::new(None)),
            attached_session: Arc::new(Mutex::new(None)),
            snapshot_seq: Arc::new(Mutex::new(0)),
            last_session_seq: Arc::new(Mutex::new(0)),
            transition_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn context(&self) -> ClientEventContext {
        let _transition = lock(&self.transition_lock);
        self.context_unlocked()
    }

    pub(crate) fn session_event_floor(&self) -> u64 {
        let _transition = lock(&self.transition_lock);
        *lock(&self.last_session_seq)
    }

    pub(crate) fn is_context_current(&self, expected: &ClientEventContext) -> bool {
        let _transition = lock(&self.transition_lock);
        self.context_unlocked() == *expected
    }

    pub(crate) fn session(&self) -> Option<SessionId> {
        let _transition = lock(&self.transition_lock);
        lock(&self.attached_session).clone()
    }

    pub(crate) fn begin_attach(
        &self,
        target: Attach,
        request_id: u64,
        idempotency_key: String,
    ) -> AttachAttempt {
        let _transition = lock(&self.transition_lock);
        let attempt = AttachAttempt {
            request_id,
            idempotency_key,
            target,
            session_epoch: self.session_epoch.fetch_add(1, Ordering::AcqRel) + 1,
            connection_epoch: self.connection_epoch.load(Ordering::Acquire),
        };
        *lock(&self.desired_attach) = Some(attempt.clone());
        *lock(&self.attached_session) = None;
        *lock(&self.snapshot_seq) = 0;
        *lock(&self.last_session_seq) = 0;
        attempt
    }

    pub(crate) fn begin_detach(&self) {
        let _transition = lock(&self.transition_lock);
        self.session_epoch.fetch_add(1, Ordering::AcqRel);
        *lock(&self.desired_attach) = None;
        *lock(&self.attached_session) = None;
        *lock(&self.snapshot_seq) = 0;
        *lock(&self.last_session_seq) = 0;
    }

    pub(crate) fn desired_attach(&self) -> Option<AttachAttempt> {
        let _transition = lock(&self.transition_lock);
        lock(&self.desired_attach).clone()
    }

    pub(crate) fn retry_attach(&self, target: &Attach, request_id: u64) -> Option<AttachAttempt> {
        let _transition = lock(&self.transition_lock);
        let desired = lock(&self.desired_attach).clone()?;
        if &desired.target != target || lock(&self.attached_session).is_some() {
            return None;
        }
        let attempt = AttachAttempt {
            request_id,
            idempotency_key: desired.idempotency_key,
            target: desired.target,
            session_epoch: desired.session_epoch,
            connection_epoch: self.connection_epoch.load(Ordering::Acquire),
        };
        *lock(&self.desired_attach) = Some(attempt.clone());
        Some(attempt)
    }

    pub(crate) fn renew_attach_request_id(&self, request_id: u64) -> Option<AttachAttempt> {
        let _transition = lock(&self.transition_lock);
        let mut attempt = lock(&self.desired_attach).clone()?;
        attempt.request_id = request_id;
        attempt.connection_epoch = self.connection_epoch.load(Ordering::Acquire);
        *lock(&self.desired_attach) = Some(attempt.clone());
        Some(attempt)
    }

    pub(crate) fn remember_if_current(
        &self,
        attempt: &AttachAttempt,
        snapshot: &SessionSnapshot,
    ) -> Option<SnapshotObservation> {
        let _transition = lock(&self.transition_lock);
        if self.connection_epoch.load(Ordering::Acquire) != attempt.connection_epoch {
            return None;
        }
        self.remember_attached_unlocked(attempt, snapshot)
    }

    pub(crate) fn remember_reconnect(
        &self,
        attempt: &AttachAttempt,
        connection_epoch: u64,
        snapshot: &SessionSnapshot,
    ) -> Option<SnapshotObservation> {
        let _transition = lock(&self.transition_lock);
        if self.connection_epoch.load(Ordering::Acquire) != connection_epoch {
            return None;
        }
        self.remember_attached_unlocked(attempt, snapshot)
    }

    pub(crate) fn clear_failed_attach(
        &self,
        attempt: &AttachAttempt,
    ) -> Option<ClientEventContext> {
        let _transition = lock(&self.transition_lock);
        if self.connection_epoch.load(Ordering::Acquire) != attempt.connection_epoch
            || !self.outgoing_attach_is_current_unlocked(attempt)
        {
            return None;
        }
        *lock(&self.desired_attach) = None;
        *lock(&self.attached_session) = None;
        *lock(&self.snapshot_seq) = 0;
        *lock(&self.last_session_seq) = 0;
        Some(self.context_unlocked())
    }

    pub(crate) fn is_attach_current(&self, attempt: &AttachAttempt, session: &SessionId) -> bool {
        let _transition = lock(&self.transition_lock);
        self.connection_epoch.load(Ordering::Acquire) == attempt.connection_epoch
            && self.session_epoch.load(Ordering::Acquire) == attempt.session_epoch
            && lock(&self.desired_attach).as_ref().is_some_and(|desired| {
                desired.request_id == attempt.request_id
                    && desired.idempotency_key == attempt.idempotency_key
            })
            && lock(&self.attached_session).as_ref() == Some(session)
    }

    pub(crate) fn accepted_event_context(
        &self,
        seq: u64,
        event: &Event,
    ) -> Option<ClientEventContext> {
        let _transition = lock(&self.transition_lock);
        if !event.is_global() {
            if lock(&self.attached_session).is_none()
                || seq <= *lock(&self.snapshot_seq)
                || seq <= *lock(&self.last_session_seq)
            {
                return None;
            }
            let mut floor = lock(&self.last_session_seq);
            *floor = seq;
        }
        Some(self.context_unlocked())
    }

    pub(crate) fn accepted_resync_context(
        &self,
        current_seq: Option<u64>,
    ) -> Option<ClientEventContext> {
        let _transition = lock(&self.transition_lock);
        let floor = (*lock(&self.snapshot_seq)).max(*lock(&self.last_session_seq));
        if lock(&self.attached_session).is_none() || current_seq.is_some_and(|seq| seq <= floor) {
            return None;
        }
        Some(self.context_unlocked())
    }

    pub(crate) fn observe_snapshot(
        &self,
        snapshot: &SessionSnapshot,
    ) -> Option<SnapshotObservation> {
        let _transition = lock(&self.transition_lock);
        if lock(&self.attached_session).as_ref() != Some(&snapshot.meta.id) {
            return None;
        }
        let mut current = lock(&self.snapshot_seq);
        let monotonic = snapshot.seq >= *current;
        if !monotonic {
            return None;
        }
        *current = (*current).max(snapshot.seq);
        let mut floor = lock(&self.last_session_seq);
        *floor = (*floor).max(snapshot.seq);
        Some(SnapshotObservation { monotonic })
    }

    pub(crate) fn advance_connection(&self) -> u64 {
        let _transition = lock(&self.transition_lock);
        self.connection_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn commit_connection(
        &self,
        attempt: Option<&AttachAttempt>,
        snapshot_id: Option<&SessionId>,
        commit: impl FnOnce(),
    ) -> Option<ClientEventContext> {
        let _transition = lock(&self.transition_lock);
        let current_attempt = lock(&self.desired_attach).clone();
        let attached = lock(&self.attached_session).clone();
        let valid = match (attempt, snapshot_id) {
            (Some(attempt), Some(snapshot_id)) => {
                current_attempt.as_ref().is_some_and(|desired| {
                    desired.request_id == attempt.request_id
                        && desired.idempotency_key == attempt.idempotency_key
                        && desired.session_epoch == attempt.session_epoch
                        && desired.connection_epoch == attempt.connection_epoch
                }) && attached.as_ref() == Some(snapshot_id)
            }
            (None, None) => current_attempt.is_none() && attached.is_none(),
            _ => false,
        };
        if !valid {
            return None;
        }
        commit();
        Some(self.context_unlocked())
    }

    fn context_unlocked(&self) -> ClientEventContext {
        ClientEventContext {
            connection_epoch: self.connection_epoch.load(Ordering::Acquire),
            session_epoch: self.session_epoch.load(Ordering::Acquire),
            session: lock(&self.attached_session).clone(),
        }
    }

    fn outgoing_attach_is_current_unlocked(&self, attempt: &AttachAttempt) -> bool {
        self.connection_epoch.load(Ordering::Acquire) == attempt.connection_epoch
            && self.desired_attempt_is_current_unlocked(attempt)
    }

    fn desired_attempt_is_current_unlocked(&self, attempt: &AttachAttempt) -> bool {
        self.session_epoch.load(Ordering::Acquire) == attempt.session_epoch
            && lock(&self.desired_attach).as_ref().is_some_and(|desired| {
                desired.request_id == attempt.request_id
                    && desired.idempotency_key == attempt.idempotency_key
                    && desired.target == attempt.target
            })
    }

    fn remember_attached_unlocked(
        &self,
        attempt: &AttachAttempt,
        snapshot: &SessionSnapshot,
    ) -> Option<SnapshotObservation> {
        if !self.desired_attempt_is_current_unlocked(attempt) {
            return None;
        }
        let mut current = lock(&self.snapshot_seq);
        let monotonic = snapshot.seq >= *current;
        *current = (*current).max(snapshot.seq);
        drop(current);
        *lock(&self.desired_attach) = Some(AttachAttempt {
            request_id: attempt.request_id,
            idempotency_key: attempt.idempotency_key.clone(),
            target: Attach::Resume {
                session: snapshot.meta.id.clone(),
            },
            session_epoch: attempt.session_epoch,
            connection_epoch: self.connection_epoch.load(Ordering::Acquire),
        });
        *lock(&self.attached_session) = Some(snapshot.meta.id.clone());
        let mut floor = lock(&self.last_session_seq);
        *floor = (*floor).max(snapshot.seq);
        Some(SnapshotObservation { monotonic })
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ClientEvent {
    Connected {
        context: ClientEventContext,
        daemon_version: String,
        pid: u32,
        /// True when the daemon runs a different binary version than this client.
        version_mismatch: bool,
    },
    Disconnected {
        context: ClientEventContext,
        reason: String,
    },
    Reconnecting {
        context: ClientEventContext,
        attempt: u32,
    },
    AttachFailed {
        context: ClientEventContext,
        attach: Attach,
        reason: String,
    },
    Snapshot {
        context: ClientEventContext,
        snapshot: SessionSnapshot,
    },
    ResyncNeeded {
        context: ClientEventContext,
    },
    Event {
        context: ClientEventContext,
        seq: u64,
        event: Event,
    },
}

const EVENT_QUEUE_CAPACITY: usize = 1024;
const EVENT_QUEUE_LIMIT: usize = EVENT_QUEUE_CAPACITY - 2;
const EVENT_QUEUE_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const EVENT_QUEUE_ADMISSION_LIMIT: usize = EVENT_QUEUE_BYTE_LIMIT - 64 * 1024;
const MAX_RESYNC_CONTEXTS: usize = 8;

#[derive(Debug)]
struct QueuedEvent {
    event: ClientEvent,
    bytes: usize,
}

#[derive(Debug)]
struct EventQueue {
    events: VecDeque<QueuedEvent>,
    queued_bytes: usize,
    resync_contexts: Vec<(ClientEventContext, u64)>,
    closed: bool,
    terminal: bool,
}

enum EventPushResult {
    Accepted,
    Dropped,
    Overflow,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GlobalEventKey {
    SessionStatus(SessionId),
    AuthUpdated(String),
    LoginProgress(String),
}

pub(crate) fn global_event_key(event: &Event) -> Option<GlobalEventKey> {
    match event {
        Event::SessionStatus { session, .. } => {
            Some(GlobalEventKey::SessionStatus(session.clone()))
        }
        Event::AuthUpdated(info) => Some(GlobalEventKey::AuthUpdated(info.id.clone())),
        Event::LoginProgress { provider, .. } => {
            Some(GlobalEventKey::LoginProgress(provider.clone()))
        }
        _ => None,
    }
}

fn client_event_bytes(event: &ClientEvent) -> usize {
    let context = event_context(event);
    let context_bytes = serde_json::to_vec(&(
        context.connection_epoch,
        context.session_epoch,
        context.session.as_ref().map(SessionId::as_str),
    ))
    .map_or(EVENT_QUEUE_BYTE_LIMIT, |bytes| bytes.len());
    let overhead = std::mem::size_of::<ClientEvent>()
        .saturating_add(std::mem::size_of::<ClientEventContext>());
    let payload_bytes = match event {
        ClientEvent::Connected {
            daemon_version,
            pid,
            version_mismatch,
            ..
        } => serde_json::to_vec(&(daemon_version, pid, version_mismatch)),
        ClientEvent::Disconnected { reason, .. } => serde_json::to_vec(reason),
        ClientEvent::Reconnecting { attempt, .. } => serde_json::to_vec(attempt),
        ClientEvent::AttachFailed { attach, reason, .. } => serde_json::to_vec(&(attach, reason)),
        ClientEvent::Snapshot { snapshot, .. } => serde_json::to_vec(snapshot),
        ClientEvent::ResyncNeeded { .. } => return context_bytes.saturating_add(overhead),
        ClientEvent::Event { seq, event, .. } => serde_json::to_vec(&(seq, event)),
    };
    payload_bytes.map_or(EVENT_QUEUE_BYTE_LIMIT, |bytes| {
        bytes
            .len()
            .saturating_add(context_bytes)
            .saturating_add(overhead)
    })
}

fn can_merge_delta(previous: &ClientEvent, next: &ClientEvent) -> bool {
    match (previous, next) {
        (
            ClientEvent::Event {
                context: previous_context,
                event:
                    Event::TextDelta {
                        agent: previous_agent,
                        item_seq: previous_item,
                        ..
                    },
                ..
            },
            ClientEvent::Event {
                context: next_context,
                event:
                    Event::TextDelta {
                        agent: next_agent,
                        item_seq: next_item,
                        ..
                    },
                ..
            },
        ) => {
            previous_context == next_context
                && previous_agent == next_agent
                && previous_item == next_item
        }
        (
            ClientEvent::Event {
                context: previous_context,
                event:
                    Event::ReasoningDelta {
                        agent: previous_agent,
                        item_seq: previous_item,
                        ..
                    },
                ..
            },
            ClientEvent::Event {
                context: next_context,
                event:
                    Event::ReasoningDelta {
                        agent: next_agent,
                        item_seq: next_item,
                        ..
                    },
                ..
            },
        ) => {
            previous_context == next_context
                && previous_agent == next_agent
                && previous_item == next_item
        }
        _ => false,
    }
}

fn merge_delta(previous: &mut ClientEvent, next: &ClientEvent) {
    match (previous, next) {
        (
            ClientEvent::Event {
                seq: previous_seq,
                event:
                    Event::TextDelta {
                        text: previous_text,
                        ..
                    },
                ..
            },
            ClientEvent::Event {
                seq: next_seq,
                event: Event::TextDelta {
                    text: next_text, ..
                },
                ..
            },
        ) => {
            previous_text.push_str(next_text);
            *previous_seq = *next_seq;
        }
        (
            ClientEvent::Event {
                seq: previous_seq,
                event:
                    Event::ReasoningDelta {
                        text: previous_text,
                        ..
                    },
                ..
            },
            ClientEvent::Event {
                seq: next_seq,
                event:
                    Event::ReasoningDelta {
                        text: next_text, ..
                    },
                ..
            },
        ) => {
            previous_text.push_str(next_text);
            *previous_seq = *next_seq;
        }
        _ => {}
    }
}

impl EventQueue {
    fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(EVENT_QUEUE_LIMIT),
            queued_bytes: 0,
            resync_contexts: Vec::new(),
            closed: false,
            terminal: false,
        }
    }

    fn push(&mut self, event: ClientEvent, resync_seq: Option<u64>) -> EventPushResult {
        if self.closed {
            return EventPushResult::Closed;
        }
        if matches!(&event, ClientEvent::Connected { .. }) {
            self.resync_contexts.clear();
            self.retain_events(|queued| !matches!(&queued.event, ClientEvent::ResyncNeeded { .. }));
        }
        if matches!(&event, ClientEvent::ResyncNeeded { .. }) {
            let context = event_context(&event);
            return if self.mark_resync(context, resync_seq.unwrap_or(0)) {
                EventPushResult::Accepted
            } else {
                EventPushResult::Overflow
            };
        }
        if is_lossy_client_event(&event) {
            let (context, seq) = match &event {
                ClientEvent::Event { context, seq, .. } => (context.clone(), *seq),
                _ => return EventPushResult::Overflow,
            };
            if let Some((_, floor)) = self
                .resync_contexts
                .iter_mut()
                .find(|(queued_context, _)| queued_context == &context)
            {
                *floor = (*floor).max(seq);
                return EventPushResult::Dropped;
            }
            let bytes = client_event_bytes(&event);
            let mergeable = self
                .events
                .back()
                .is_some_and(|queued| can_merge_delta(&queued.event, &event))
                && self.queued_bytes.saturating_add(bytes) <= EVENT_QUEUE_BYTE_LIMIT;
            if mergeable {
                let (previous_bytes, merged_bytes) = {
                    let Some(queued) = self.events.back_mut() else {
                        return EventPushResult::Overflow;
                    };
                    let previous_bytes = queued.bytes;
                    merge_delta(&mut queued.event, &event);
                    queued.bytes = client_event_bytes(&queued.event);
                    (previous_bytes, queued.bytes)
                };
                self.queued_bytes = if merged_bytes >= previous_bytes {
                    self.queued_bytes
                        .saturating_add(merged_bytes - previous_bytes)
                } else {
                    self.queued_bytes
                        .saturating_sub(previous_bytes - merged_bytes)
                };
                return if self.queued_bytes <= EVENT_QUEUE_BYTE_LIMIT {
                    EventPushResult::Accepted
                } else {
                    EventPushResult::Overflow
                };
            }
            if self.events.len() < EVENT_QUEUE_LIMIT
                && self.queued_bytes.saturating_add(bytes) <= EVENT_QUEUE_ADMISSION_LIMIT
            {
                self.events.push_back(QueuedEvent { event, bytes });
                self.queued_bytes = self.queued_bytes.saturating_add(bytes);
                return EventPushResult::Accepted;
            }
            return if self.mark_resync(context, seq) {
                EventPushResult::Dropped
            } else {
                EventPushResult::Overflow
            };
        }
        let bytes = client_event_bytes(&event);
        if let ClientEvent::Event { event: inner, .. } = &event
            && let Some(key) = global_event_key(inner)
            && let Some(index) = self.events.iter().position(|queued| {
                matches!(
                    &queued.event,
                    ClientEvent::Event { event: queued, .. } if global_event_key(queued).as_ref() == Some(&key)
                )
            })
        {
            let previous_bytes = self.events[index].bytes;
            if self
                .queued_bytes
                .saturating_sub(previous_bytes)
                .saturating_add(bytes)
                > EVENT_QUEUE_BYTE_LIMIT
            {
                return EventPushResult::Overflow;
            }
            let _ = self.remove_at(index);
            self.events.push_back(QueuedEvent { event, bytes });
            self.queued_bytes = self
                .queued_bytes
                .saturating_add(bytes);
            return EventPushResult::Accepted;
        }
        if self.events.len() < EVENT_QUEUE_LIMIT
            && self.queued_bytes.saturating_add(bytes) <= EVENT_QUEUE_ADMISSION_LIMIT
        {
            self.events.push_back(QueuedEvent { event, bytes });
            self.queued_bytes = self.queued_bytes.saturating_add(bytes);
            return EventPushResult::Accepted;
        }
        while self.events.len() >= EVENT_QUEUE_CAPACITY
            || self.queued_bytes.saturating_add(bytes) > EVENT_QUEUE_BYTE_LIMIT
        {
            let Some(index) = self
                .events
                .iter()
                .position(|queued| is_lossy_client_event(&queued.event))
            else {
                return EventPushResult::Overflow;
            };
            let _ = self.remove_at(index);
        }
        self.events.push_back(QueuedEvent { event, bytes });
        self.queued_bytes = self.queued_bytes.saturating_add(bytes);
        EventPushResult::Accepted
    }

    fn remove_at(&mut self, index: usize) -> Option<QueuedEvent> {
        let removed = self.events.remove(index)?;
        self.queued_bytes = self.queued_bytes.saturating_sub(removed.bytes);
        Some(removed)
    }

    fn retain_events(&mut self, mut keep: impl FnMut(&QueuedEvent) -> bool) {
        let mut queued_bytes: usize = 0;
        self.events.retain_mut(|queued| {
            if keep(queued) {
                queued_bytes = queued_bytes.saturating_add(queued.bytes);
                true
            } else {
                false
            }
        });
        self.queued_bytes = queued_bytes;
    }

    fn mark_resync(&mut self, context: ClientEventContext, seq: u64) -> bool {
        if let Some((_, floor)) = self
            .resync_contexts
            .iter_mut()
            .find(|(queued_context, _)| queued_context == &context)
        {
            *floor = (*floor).max(seq);
            return true;
        }
        if self.resync_contexts.len() >= MAX_RESYNC_CONTEXTS {
            return false;
        }
        let event = ClientEvent::ResyncNeeded {
            context: context.clone(),
        };
        let bytes = client_event_bytes(&event);
        while self.events.len() >= EVENT_QUEUE_CAPACITY
            || self.queued_bytes.saturating_add(bytes) > EVENT_QUEUE_BYTE_LIMIT
        {
            let Some(index) = self
                .events
                .iter()
                .position(|queued| is_lossy_client_event(&queued.event))
            else {
                return false;
            };
            let _ = self.remove_at(index);
        }
        self.events.push_back(QueuedEvent { event, bytes });
        self.queued_bytes = self.queued_bytes.saturating_add(bytes);
        self.resync_contexts.push((context, seq));
        true
    }

    fn snapshot_applied(&mut self, session: &SessionId, seq: u64) {
        let live_resync_contexts = self
            .resync_contexts
            .iter()
            .filter(|(context, floor)| context.session.as_ref() == Some(session) && *floor > seq)
            .map(|(context, _)| context.clone())
            .collect::<Vec<_>>();
        self.retain_events(|queued| match &queued.event {
            ClientEvent::ResyncNeeded { context } if context.session.as_ref() == Some(session) => {
                live_resync_contexts.contains(context)
            }
            ClientEvent::Event {
                context,
                seq: event_seq,
                event,
            } => event.is_global() || context.session.as_ref() != Some(session) || *event_seq > seq,
            _ => true,
        });
        self.resync_contexts
            .retain(|(context, floor)| context.session.as_ref() != Some(session) || *floor > seq);
    }

    fn force_terminal(&mut self, event: ClientEvent) {
        if self.terminal {
            return;
        }
        let bytes = client_event_bytes(&event);
        while self.events.len() >= EVENT_QUEUE_CAPACITY
            || self.queued_bytes.saturating_add(bytes) > EVENT_QUEUE_BYTE_LIMIT
        {
            let index = self
                .events
                .iter()
                .position(|queued| is_lossy_client_event(&queued.event))
                .unwrap_or(0);
            let _ = self.remove_at(index);
        }
        self.events.push_back(QueuedEvent { event, bytes });
        self.queued_bytes = self.queued_bytes.saturating_add(bytes);
        self.terminal = true;
        self.closed = true;
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

fn is_lossy_client_event(event: &ClientEvent) -> bool {
    matches!(event, ClientEvent::Event { event, .. } if event.is_lossy_delta())
}

fn event_context(event: &ClientEvent) -> ClientEventContext {
    match event {
        ClientEvent::Connected { context, .. }
        | ClientEvent::Disconnected { context, .. }
        | ClientEvent::Reconnecting { context, .. }
        | ClientEvent::AttachFailed { context, .. }
        | ClientEvent::Snapshot { context, .. }
        | ClientEvent::ResyncNeeded { context }
        | ClientEvent::Event { context, .. } => context.clone(),
    }
}

#[derive(Clone)]
pub(crate) struct EventSink {
    queue: Arc<Mutex<EventQueue>>,
    notify: Arc<Notify>,
    cancel_token: CancellationToken,
}

impl EventSink {
    pub(crate) fn channel(cancel_token: CancellationToken) -> (Self, EventReceiver) {
        let queue = Arc::new(Mutex::new(EventQueue::new()));
        let notify = Arc::new(Notify::new());
        let receiver_cancel = cancel_token.clone();
        (
            Self {
                queue: queue.clone(),
                notify: notify.clone(),
                cancel_token,
            },
            EventReceiver {
                queue,
                notify,
                cancel_token: receiver_cancel,
            },
        )
    }

    pub(crate) fn emit(&self, event: ClientEvent) -> bool {
        self.emit_with_resync(event, None)
    }

    pub(crate) fn emit_resync(
        &self,
        context: ClientEventContext,
        current_seq: Option<u64>,
    ) -> bool {
        self.emit_with_resync(ClientEvent::ResyncNeeded { context }, current_seq)
    }

    fn emit_with_resync(&self, event: ClientEvent, resync_seq: Option<u64>) -> bool {
        if self.cancel_token.is_cancelled() {
            return true;
        }
        let context = event_context(&event);
        let result = lock(&self.queue).push(event, resync_seq);
        match result {
            EventPushResult::Accepted | EventPushResult::Dropped => {
                self.notify.notify_one();
                true
            }
            EventPushResult::Overflow => {
                lock(&self.queue).force_terminal(ClientEvent::Disconnected {
                    context,
                    reason: "client event queue overflow".into(),
                });
                self.notify.notify_waiters();
                false
            }
            EventPushResult::Closed => false,
        }
    }

    pub(crate) fn terminate(&self, event: ClientEvent) {
        if self.cancel_token.is_cancelled() {
            return;
        }
        lock(&self.queue).force_terminal(event);
        self.notify.notify_waiters();
    }

    pub(crate) fn snapshot_applied(&self, session: &SessionId, seq: u64) {
        lock(&self.queue).snapshot_applied(session, seq);
    }

    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.queue).closed
    }
}

#[derive(Debug)]
pub struct EventReceiver {
    queue: Arc<Mutex<EventQueue>>,
    notify: Arc<Notify>,
    cancel_token: CancellationToken,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        loop {
            let notified = self.notify.notified();
            let next = {
                let mut queue = lock(&self.queue);
                match queue.events.pop_front() {
                    Some(queued) => {
                        queue.queued_bytes = queue.queued_bytes.saturating_sub(queued.bytes);
                        Some(Some(queued.event))
                    }
                    None if queue.closed => Some(None),
                    None => None,
                }
            };
            if let Some(event) = next {
                return event;
            }
            tokio::select! {
                _ = self.cancel_token.cancelled() => return None,
                _ = notified => {}
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<ClientEvent> {
        let mut queue = lock(&self.queue);
        let queued = queue.events.pop_front()?;
        queue.queued_bytes = queue.queued_bytes.saturating_sub(queued.bytes);
        Some(queued.event)
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        lock(&self.queue).close();
    }
}

#[cfg(test)]
mod event_queue_tests {
    use super::*;

    #[test]
    fn event_queue_enforces_byte_limit_without_unbounded_allocation() {
        let cancel = CancellationToken::new();
        let (sink, mut receiver) = EventSink::channel(cancel);
        let context = ClientEventContext::unscoped();
        let payload = "x".repeat(EVENT_QUEUE_BYTE_LIMIT - 4096);
        assert!(sink.emit(ClientEvent::Event {
            context: context.clone(),
            seq: 1,
            event: Event::Toast {
                level: pacode_types::state::ToastLevel::Info,
                title: "large".into(),
                detail: Some(payload),
            },
        }));
        assert!(!sink.emit(ClientEvent::Event {
            context,
            seq: 2,
            event: Event::Toast {
                level: pacode_types::state::ToastLevel::Info,
                title: "overflow".into(),
                detail: Some("x".repeat(EVENT_QUEUE_BYTE_LIMIT / 4)),
            },
        }));
        let mut saw_control = false;
        let reason = loop {
            match receiver.try_recv() {
                Some(ClientEvent::Disconnected { reason, .. }) => break reason,
                Some(ClientEvent::Event {
                    event: Event::Toast { .. },
                    ..
                }) => saw_control = true,
                Some(_) => {}
                None => panic!("event queue closed without overflow notification"),
            }
        };
        assert!(saw_control);
        assert_eq!(reason, "client event queue overflow");
        assert!(receiver.try_recv().is_none());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to daemon at {path}: {source}")]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to start daemon: {0}")]
    Spawn(String),
    #[error("protocol mismatch: daemon speaks {daemon}, client {client}")]
    Protocol { daemon: u32, client: u32 },
    #[error("disconnected")]
    Disconnected,
    #[error("request belongs to a stale client or session epoch")]
    StaleSession,
    #[error("request sequence cannot contain connection transition: {request}")]
    InvalidSequenceRequest { request: &'static str },
    #[error("request timed out: {0}")]
    Timeout(&'static str),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("unexpected reply to {request}: {reply}")]
    UnexpectedReply { request: String, reply: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Typed helpers over `Client::request` used by every frontend.
pub trait ClientExt {
    fn attached_session(&self) -> Option<SessionId>;
}

// Re-exports for frontends that only need the wire types.
pub use pacode_types::{
    Attach as AttachRequest, McpServerInfo, PluginCommandOutcome, PluginInfo, Reply as WireReply,
    Request as WireRequest,
};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[allow(dead_code)]
fn _assert_types(_: Attach, _: Request, _: Reply) {}
