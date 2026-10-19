use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pacode_types::ids::ClientId;
use pacode_types::{
    Attach, AttachEnvelope, ClientHello, Envelope, Event, PROTOCOL_VERSION, Reply, Request,
    ServerMessage, SessionSnapshot,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    AttachAttempt, ClientError, ClientEvent, ClientOptions, EventSink, GlobalEventKey,
    LifecycleState, SnapshotObservation, global_event_key, lock,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterStatus {
    Running,
    Failed,
}

pub(crate) struct WriterGeneration {
    sender: mpsc::Sender<String>,
    status: watch::Receiver<WriterStatus>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl WriterGeneration {
    pub(crate) fn sender(&self) -> mpsc::Sender<String> {
        self.sender.clone()
    }

    fn status(&mut self) -> &mut watch::Receiver<WriterStatus> {
        &mut self.status
    }

    async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.handle.await {
            log::warn!("writer task failed during shutdown: {error}");
        }
    }
}

pub(crate) fn spawn_writer(
    write_half: OwnedWriteHalf,
    cancel_token: CancellationToken,
    write_timeout: std::time::Duration,
) -> WriterGeneration {
    let (sender, receiver) = mpsc::channel(128);
    let writer_cancel = cancel_token.child_token();
    let (failed, status) = watch::channel(WriterStatus::Running);
    let handle = tokio::spawn(run_writer(
        write_half,
        receiver,
        writer_cancel.clone(),
        failed,
        write_timeout,
    ));
    WriterGeneration {
        sender,
        status,
        cancel: writer_cancel,
        handle,
    }
}

async fn run_writer<W>(
    mut write_half: W,
    mut receiver: mpsc::Receiver<String>,
    cancel_token: CancellationToken,
    failed: watch::Sender<WriterStatus>,
    write_timeout: std::time::Duration,
) where
    W: AsyncWrite + Unpin,
{
    loop {
        let line = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return,
            message = receiver.recv() => {
                let Some(message) = message else { return; };
                message
            }
        };
        let deadline = tokio::time::Instant::now() + write_timeout;
        let result = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return,
            result = tokio::time::timeout_at(deadline, async {
                write_half.write_all(line.as_bytes()).await?;
                write_half.flush().await
            }) => result,
        };
        let Ok(Ok(())) = result else {
            let _ = failed.send(WriterStatus::Failed);
            return;
        };
    }
}

pub(crate) async fn emit_event(
    events_tx: &EventSink,
    _dropped_events: &AtomicUsize,
    event: ClientEvent,
) {
    events_tx.emit(event);
}

pub(crate) fn fail_pending(
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pending_attaches: &Arc<Mutex<HashMap<u64, AttachAttempt>>>,
) {
    lock(pending).clear();
    lock(pending_attaches).clear();
}

const HANDSHAKE_EVENT_CAPACITY: usize = 128;
const HANDSHAKE_EVENT_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const MAX_NDJSON_LINE: usize = 8 * 1024 * 1024;

pub(super) enum HandshakeError {
    Cancelled,
    Timeout,
    Closed,
    Io(std::io::Error),
    Json(serde_json::Error),
    Daemon(String),
    Protocol { daemon: u32 },
    EventBufferOverflow,
    LineOverflow,
    Unexpected,
}

pub(super) enum BufferedServerEvent {
    Event { seq: u64, event: Box<Event> },
    Resync { current_seq: Option<u64> },
}

#[derive(Default)]
pub(super) struct BufferedEvents {
    events: VecDeque<BufferedServerEvent>,
    queued_bytes: usize,
    resync_needed: bool,
    resync_seq: Option<u64>,
    overflowed: bool,
}

fn buffered_event_bytes(event: &BufferedServerEvent) -> usize {
    let serialized = match event {
        BufferedServerEvent::Event { seq, event } => serde_json::to_vec(&(seq, event.as_ref())),
        BufferedServerEvent::Resync { .. } => return std::mem::size_of::<BufferedServerEvent>(),
    };
    serialized.map_or(HANDSHAKE_EVENT_BYTE_LIMIT, |bytes| {
        bytes
            .len()
            .saturating_add(std::mem::size_of::<BufferedServerEvent>())
    })
}

fn buffered_global_key(event: &BufferedServerEvent) -> Option<GlobalEventKey> {
    match event {
        BufferedServerEvent::Event { event, .. } => global_event_key(event.as_ref()),
        BufferedServerEvent::Resync { .. } => None,
    }
}

impl BufferedEvents {
    fn push(&mut self, event: BufferedServerEvent) -> bool {
        match &event {
            BufferedServerEvent::Resync { current_seq } => {
                self.mark_resync(*current_seq);
                true
            }
            BufferedServerEvent::Event { event, .. } if !event.is_global() => {
                self.mark_resync(None);
                true
            }
            _ => {
                let bytes = buffered_event_bytes(&event);
                if let Some(key) = buffered_global_key(&event)
                    && let Some(index) = self
                        .events
                        .iter()
                        .position(|queued| buffered_global_key(queued).as_ref() == Some(&key))
                {
                    let previous_bytes = buffered_event_bytes(&self.events[index]);
                    if self
                        .queued_bytes
                        .saturating_sub(previous_bytes)
                        .saturating_add(bytes)
                        > HANDSHAKE_EVENT_BYTE_LIMIT
                    {
                        self.overflowed = true;
                        return false;
                    }
                    if let Some(removed) = self.events.remove(index) {
                        self.queued_bytes = self
                            .queued_bytes
                            .saturating_sub(buffered_event_bytes(&removed));
                    }
                }
                if self.events.len() >= HANDSHAKE_EVENT_CAPACITY
                    || self.queued_bytes.saturating_add(bytes) > HANDSHAKE_EVENT_BYTE_LIMIT
                {
                    self.overflowed = true;
                    return false;
                }
                self.events.push_back(event);
                self.queued_bytes = self.queued_bytes.saturating_add(bytes);
                true
            }
        }
    }

    fn merge(&mut self, other: Self) {
        self.overflowed |= other.overflowed;
        if other.resync_needed {
            self.resync_needed = true;
            self.resync_seq = match (self.resync_seq, other.resync_seq) {
                (_, None) => None,
                (None, Some(seq)) => Some(seq),
                (Some(current), Some(seq)) => Some(current.max(seq)),
            };
        }
        for event in other.events {
            if !self.push(event) {
                break;
            }
        }
    }

    fn mark_resync(&mut self, current_seq: Option<u64>) {
        self.resync_needed = true;
        self.resync_seq = match (self.resync_seq, current_seq) {
            (_, None) => None,
            (None, Some(seq)) => Some(seq),
            (Some(current), Some(seq)) => Some(current.max(seq)),
        };
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub(super) fn into_events(self) -> VecDeque<BufferedServerEvent> {
        self.events
    }

    fn into_parts(self) -> (VecDeque<BufferedServerEvent>, bool, Option<u64>, bool) {
        (
            self.events,
            self.resync_needed,
            self.resync_seq,
            self.overflowed,
        )
    }
}

pub(super) struct HandshakeFailure {
    pub(super) kind: HandshakeError,
    pub(super) events: BufferedEvents,
}

impl HandshakeFailure {
    fn new(kind: HandshakeError, events: BufferedEvents) -> Self {
        Self { kind, events }
    }
}

enum NdjsonLineError {
    Io(std::io::Error),
    Overflow,
}

async fn read_ndjson_line<R>(reader: &mut R, line: &mut Vec<u8>) -> Result<usize, NdjsonLineError>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    loop {
        let (consumed, done, overflow) = {
            let available = reader.fill_buf().await.map_err(NdjsonLineError::Io)?;
            if available.is_empty() {
                return Ok(line.len());
            }
            match available.iter().position(|&byte| byte == b'\n') {
                Some(index) => {
                    if line.len().saturating_add(index) > MAX_NDJSON_LINE {
                        (index + 1, true, true)
                    } else {
                        line.extend_from_slice(&available[..=index]);
                        (index + 1, true, false)
                    }
                }
                None => {
                    let count = available.len();
                    if line.len().saturating_add(count) > MAX_NDJSON_LINE {
                        (count, true, true)
                    } else {
                        line.extend_from_slice(available);
                        (count, false, false)
                    }
                }
            }
        };
        reader.consume(consumed);
        if overflow {
            return Err(NdjsonLineError::Overflow);
        }
        if done {
            return Ok(line.len());
        }
    }
}

fn line_overflow_message() -> String {
    format!("NDJSON line too long: exceeds {MAX_NDJSON_LINE} bytes")
}

pub(super) fn client_error(error: HandshakeError) -> ClientError {
    match error {
        HandshakeError::Cancelled | HandshakeError::Closed => ClientError::Disconnected,
        HandshakeError::Timeout => ClientError::Timeout("hello"),
        HandshakeError::Io(error) => ClientError::Io(error),
        HandshakeError::Json(error) => ClientError::Json(error),
        HandshakeError::Daemon(message) => ClientError::Daemon(message),
        HandshakeError::Protocol { daemon } => ClientError::Protocol {
            daemon,
            client: PROTOCOL_VERSION,
        },
        HandshakeError::EventBufferOverflow => ClientError::UnexpectedReply {
            request: "handshake".into(),
            reply: "event buffer overflow".into(),
        },
        HandshakeError::LineOverflow => ClientError::UnexpectedReply {
            request: "handshake".into(),
            reply: line_overflow_message(),
        },
        HandshakeError::Unexpected => ClientError::UnexpectedReply {
            request: "hello".into(),
            reply: "unexpected server message".into(),
        },
    }
}

fn is_terminal_handshake_error(error: &HandshakeError) -> bool {
    matches!(
        error,
        HandshakeError::Json(_)
            | HandshakeError::Daemon(_)
            | HandshakeError::Protocol { .. }
            | HandshakeError::EventBufferOverflow
            | HandshakeError::LineOverflow
            | HandshakeError::Unexpected
    )
}

pub(super) async fn write_handshake(
    write_half: &mut OwnedWriteHalf,
    envelope: &Envelope,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<(), HandshakeError> {
    let mut line = serde_json::to_string(envelope).map_err(HandshakeError::Json)?;
    line.push('\n');
    write_serialized_handshake(write_half, &line, timeout, cancel_token).await
}

pub(super) async fn write_attach_handshake(
    write_half: &mut OwnedWriteHalf,
    envelope: &AttachEnvelope,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<(), HandshakeError> {
    let mut line = serde_json::to_string(envelope).map_err(HandshakeError::Json)?;
    line.push('\n');
    write_serialized_handshake(write_half, &line, timeout, cancel_token).await
}

async fn write_serialized_handshake(
    write_half: &mut OwnedWriteHalf,
    line: &str,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<(), HandshakeError> {
    let deadline = tokio::time::Instant::now() + timeout;
    io_with_deadline(
        deadline,
        cancel_token,
        write_half.write_all(line.as_bytes()),
    )
    .await?;
    io_with_deadline(deadline, cancel_token, write_half.flush()).await
}

pub(super) async fn read_reply_handshake(
    reader: &mut BufReader<OwnedReadHalf>,
    expected_id: u64,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<(Reply, BufferedEvents), HandshakeFailure> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut events = BufferedEvents::default();
    let mut line = Vec::new();
    loop {
        let read = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Err(HandshakeFailure::new(HandshakeError::Cancelled, events));
            }
            result = tokio::time::timeout_at(deadline, read_ndjson_line(reader, &mut line)) => {
                match result {
                    Ok(Ok(0)) => {
                        return Err(HandshakeFailure::new(HandshakeError::Closed, events));
                    }
                    Ok(Ok(read)) => read,
                    Ok(Err(NdjsonLineError::Io(error))) => {
                        return Err(HandshakeFailure::new(HandshakeError::Io(error), events));
                    }
                    Ok(Err(NdjsonLineError::Overflow)) => {
                        return Err(HandshakeFailure::new(HandshakeError::LineOverflow, events));
                    }
                    Err(_) => {
                        return Err(HandshakeFailure::new(HandshakeError::Timeout, events));
                    }
                }
            }
        };
        if read == 0 {
            return Err(HandshakeFailure::new(HandshakeError::Closed, events));
        }
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let message = match serde_json::from_slice::<ServerMessage>(&line) {
            Ok(message) => message,
            Err(error) => {
                return Err(HandshakeFailure::new(HandshakeError::Json(error), events));
            }
        };
        match message {
            ServerMessage::Reply { id, reply } if id == expected_id => {
                return Ok((reply, events));
            }
            ServerMessage::Reply { .. } => {
                return Err(HandshakeFailure::new(HandshakeError::Unexpected, events));
            }
            ServerMessage::Event { seq, event } => {
                if !events.push(BufferedServerEvent::Event {
                    seq,
                    event: Box::new(event),
                }) {
                    return Err(HandshakeFailure::new(
                        HandshakeError::EventBufferOverflow,
                        events,
                    ));
                }
            }
            ServerMessage::Resync { current_seq } => {
                events.push(BufferedServerEvent::Resync { current_seq });
            }
        }
    }
}

async fn io_with_deadline<F>(
    deadline: tokio::time::Instant,
    cancel_token: &CancellationToken,
    future: F,
) -> Result<(), HandshakeError>
where
    F: Future<Output = std::io::Result<()>>,
{
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    tokio::select! {
        _ = cancel_token.cancelled() => Err(HandshakeError::Cancelled),
        result = tokio::time::timeout(remaining, future) => match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(HandshakeError::Io(error)),
            Err(_) => Err(HandshakeError::Timeout),
        },
    }
}

enum ReaderExit {
    Cancelled,
    Retryable(String),
    Terminal(String),
}

async fn run_reader(
    reader: &mut BufReader<OwnedReadHalf>,
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pending_attaches: &Arc<Mutex<HashMap<u64, AttachAttempt>>>,
    events_tx: &EventSink,
    lifecycle: &LifecycleState,
    writer_status: &mut watch::Receiver<WriterStatus>,
    cancel_token: &CancellationToken,
) -> ReaderExit {
    let mut line = Vec::new();
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => return ReaderExit::Cancelled,
            _ = writer_status.changed() => {
                let reason = match &*writer_status.borrow() {
                    WriterStatus::Running => "writer exited",
                    WriterStatus::Failed => "writer failed",
                };
                return ReaderExit::Retryable(reason.into());
            }
            result = read_ndjson_line(reader, &mut line) => {
                match result {
                    Ok(0) => return ReaderExit::Retryable("server closed connection".into()),
                    Ok(_) => {
                        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                            continue;
                        }
                        match serde_json::from_slice::<ServerMessage>(&line) {
                            Ok(ServerMessage::Reply { id, reply }) => {
                                let attempt = lock(pending_attaches).remove(&id);
                                let tx = lock(pending).remove(&id);
                                if let Reply::Attached(snapshot) = &reply {
                                    let observation = attempt
                                        .as_ref()
                                        .zip(tx.as_ref())
                                        .and_then(|(attempt, tx)| {
                                            if tx.is_closed() {
                                                None
                                            } else {
                                                lifecycle.remember_if_current(attempt, snapshot)
                                            }
                                        });
                                    if let Some(observation) = observation {
                                        if tx
                                            .as_ref()
                                            .is_some_and(|tx| tx.is_closed())
                                            && let Some(attempt) = attempt.as_ref()
                                        {
                                            super::clear_unacknowledged_attach_state(
                                                lifecycle,
                                                attempt,
                                            );
                                        }
                                        if observation.monotonic
                                            && lifecycle.session().as_ref()
                                                == Some(&snapshot.meta.id)
                                        {
                                            events_tx.snapshot_applied(
                                                &snapshot.meta.id,
                                                snapshot.seq,
                                            );
                                        }
                                    }
                                }
                                if let Reply::Snapshot(snapshot) = &reply
                                    && let Some(observation) =
                                        lifecycle.observe_snapshot(snapshot)
                                    && observation.monotonic
                                    && lifecycle.session().as_ref() == Some(&snapshot.meta.id)
                                {
                                    events_tx
                                        .snapshot_applied(&snapshot.meta.id, snapshot.seq);
                                }
                                if let Some(tx) = tx {
                                    let _ = tx.send(reply);
                                }
                            }
                            Ok(ServerMessage::Event { seq, event }) => {
                                if let Some(context) = lifecycle.accepted_event_context(seq, &event)
                                    && !events_tx.emit(ClientEvent::Event { context, seq, event })
                                {
                                    return ReaderExit::Retryable("client event queue closed".into());
                                }
                            }
                            Ok(ServerMessage::Resync { current_seq }) => {
                                if let Some(context) = lifecycle.accepted_resync_context(current_seq)
                                    && !events_tx.emit_resync(context, current_seq)
                                {
                                    return ReaderExit::Retryable("client event queue closed".into());
                                }
                            }
                            Err(error) => {
                                log::error!(
                                    "protocol error: failed to deserialize ServerMessage: {error} ({} bytes)",
                                    line.len()
                                );
                            }
                        }
                    }
                    Err(NdjsonLineError::Io(error)) => {
                        return ReaderExit::Retryable(format!("read error: {error}"));
                    }
                    Err(NdjsonLineError::Overflow) => {
                        return ReaderExit::Terminal(line_overflow_message());
                    }
                }
            }
        }
    }
}

fn next_backoff(cur: std::time::Duration) -> std::time::Duration {
    match cur.as_millis() {
        500 => std::time::Duration::from_secs(1),
        1000 => std::time::Duration::from_secs(2),
        2000 => std::time::Duration::from_secs(4),
        _ => std::time::Duration::from_secs(8),
    }
}

async fn do_reconnect_hello(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    client_id: &ClientId,
    app_version: &str,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<(String, u32, bool, BufferedEvents), HandshakeFailure> {
    let hello = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: client_id.clone(),
            app_version: app_version.to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    write_handshake(write_half, &hello, timeout, cancel_token)
        .await
        .map_err(|kind| HandshakeFailure::new(kind, BufferedEvents::default()))?;
    let (reply, events) = read_reply_handshake(reader, 1, timeout, cancel_token).await?;
    match reply {
        Reply::Hello {
            daemon_version,
            protocol,
            pid,
        } if protocol == PROTOCOL_VERSION => {
            let version_mismatch = daemon_version != app_version;
            Ok((daemon_version, pid, version_mismatch, events))
        }
        Reply::Hello { protocol, .. } => Err(HandshakeFailure::new(
            HandshakeError::Protocol { daemon: protocol },
            events,
        )),
        Reply::Error { message } => Err(HandshakeFailure::new(
            HandshakeError::Daemon(message),
            events,
        )),
        _ => Err(HandshakeFailure::new(HandshakeError::Unexpected, events)),
    }
}

enum ReconnectAttachOutcome {
    Attached {
        snapshot: Box<SessionSnapshot>,
        events: BufferedEvents,
    },
    Error {
        message: String,
        events: BufferedEvents,
    },
    Retry(BufferedEvents),
    Fatal(HandshakeFailure),
}

enum ReconnectDetachOutcome {
    Detached(BufferedEvents),
    Retry(BufferedEvents),
    Fatal(HandshakeFailure),
}

enum ReconnectAttachResult {
    Attached {
        attempt: AttachAttempt,
        snapshot: Box<SessionSnapshot>,
        observation: SnapshotObservation,
    },
    Detached,
    Retry,
    Fatal {
        reason: String,
    },
    Error {
        attempt: AttachAttempt,
        target: Attach,
        reason: String,
    },
}

struct AttachRetryCtx<'a> {
    write_half: &'a mut OwnedWriteHalf,
    reader: &'a mut BufReader<OwnedReadHalf>,
    lifecycle: &'a LifecycleState,
    pending_events: &'a mut BufferedEvents,
    next_id: &'a AtomicU64,
    timeout: std::time::Duration,
    cancel_token: &'a CancellationToken,
}

async fn reconnect_attach_until_desired(
    ctx: AttachRetryCtx<'_>,
    mut attempt: AttachAttempt,
    connection_epoch: u64,
) -> ReconnectAttachResult {
    loop {
        match do_reconnect_attach(
            ctx.write_half,
            ctx.reader,
            &attempt,
            ctx.timeout,
            ctx.cancel_token,
        )
        .await
        {
            ReconnectAttachOutcome::Attached { snapshot, events } => {
                ctx.pending_events.merge(events);
                if ctx.pending_events.overflowed {
                    return ReconnectAttachResult::Fatal {
                        reason: "client handshake event buffer overflow".into(),
                    };
                }
                if let Some(observation) =
                    ctx.lifecycle
                        .remember_reconnect(&attempt, connection_epoch, &snapshot)
                {
                    attempt.connection_epoch = connection_epoch;
                    return ReconnectAttachResult::Attached {
                        attempt,
                        snapshot,
                        observation,
                    };
                }
                let next = match ctx.lifecycle.desired_attach() {
                    Some(next) => next,
                    None => {
                        let detach_id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
                        match do_reconnect_detach(
                            ctx.write_half,
                            ctx.reader,
                            detach_id,
                            ctx.timeout,
                            ctx.cancel_token,
                        )
                        .await
                        {
                            ReconnectDetachOutcome::Detached(events) => {
                                ctx.pending_events.merge(events);
                                if ctx.pending_events.overflowed {
                                    return ReconnectAttachResult::Fatal {
                                        reason: "client handshake event buffer overflow".into(),
                                    };
                                }
                                let Some(next) = ctx.lifecycle.desired_attach() else {
                                    return ReconnectAttachResult::Detached;
                                };
                                next
                            }
                            ReconnectDetachOutcome::Retry(events) => {
                                ctx.pending_events.merge(events);
                                if ctx.pending_events.overflowed {
                                    return ReconnectAttachResult::Fatal {
                                        reason: "client handshake event buffer overflow".into(),
                                    };
                                }
                                return ReconnectAttachResult::Retry;
                            }
                            ReconnectDetachOutcome::Fatal(failure) => {
                                return ReconnectAttachResult::Fatal {
                                    reason: client_error(failure.kind).to_string(),
                                };
                            }
                        }
                    }
                };
                if next.request_id == attempt.request_id
                    && next.idempotency_key == attempt.idempotency_key
                    && next.session_epoch == attempt.session_epoch
                {
                    return ReconnectAttachResult::Retry;
                }
                attempt = next;
            }
            ReconnectAttachOutcome::Error { message, events } => {
                ctx.pending_events.merge(events);
                return ReconnectAttachResult::Error {
                    target: attempt.target.clone(),
                    attempt,
                    reason: message,
                };
            }
            ReconnectAttachOutcome::Retry(events) => {
                ctx.pending_events.merge(events);
                return ReconnectAttachResult::Retry;
            }
            ReconnectAttachOutcome::Fatal(failure) => {
                return ReconnectAttachResult::Fatal {
                    reason: client_error(failure.kind).to_string(),
                };
            }
        }
    }
}

async fn do_reconnect_attach(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    attempt: &AttachAttempt,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> ReconnectAttachOutcome {
    let envelope = AttachEnvelope::new(
        attempt.request_id,
        attempt.target.clone(),
        attempt.idempotency_key.clone(),
    );
    if let Err(kind) = write_attach_handshake(write_half, &envelope, timeout, cancel_token).await {
        return if is_terminal_handshake_error(&kind) {
            ReconnectAttachOutcome::Fatal(HandshakeFailure::new(kind, BufferedEvents::default()))
        } else {
            ReconnectAttachOutcome::Retry(BufferedEvents::default())
        };
    }
    let (reply, events) =
        match read_reply_handshake(reader, attempt.request_id, timeout, cancel_token).await {
            Ok(result) => result,
            Err(failure) => {
                return if is_terminal_handshake_error(&failure.kind) {
                    ReconnectAttachOutcome::Fatal(failure)
                } else {
                    ReconnectAttachOutcome::Retry(failure.events)
                };
            }
        };
    match reply {
        Reply::Attached(snapshot) => ReconnectAttachOutcome::Attached {
            snapshot: Box::new(snapshot),
            events,
        },
        Reply::Error { message } => ReconnectAttachOutcome::Error { message, events },
        _ => {
            ReconnectAttachOutcome::Fatal(HandshakeFailure::new(HandshakeError::Unexpected, events))
        }
    }
}

/// Replay the caller's `Subscribe` on a new connection. The daemon keeps interest per
/// connection, so without this a reconnected client would silently get every agent's
/// stream again. It goes out before the attach, so the new session forwarder filters
/// from its first event. A daemon that refuses it leaves the default (everything),
/// which costs volume, never correctness.
async fn do_reconnect_subscribe(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    request_id: u64,
    request: Request,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<BufferedEvents, HandshakeFailure> {
    let envelope = Envelope {
        id: request_id,
        req: request,
    };
    write_handshake(write_half, &envelope, timeout, cancel_token)
        .await
        .map_err(|kind| HandshakeFailure::new(kind, BufferedEvents::default()))?;
    let (reply, events) = read_reply_handshake(reader, request_id, timeout, cancel_token).await?;
    if let Reply::Error { message } = reply {
        log::warn!("daemon refused the replayed subscription: {message}");
    }
    Ok(events)
}

async fn do_reconnect_detach(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    request_id: u64,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> ReconnectDetachOutcome {
    let envelope = Envelope {
        id: request_id,
        req: Request::Detach,
    };
    if let Err(kind) = write_handshake(write_half, &envelope, timeout, cancel_token).await {
        return if is_terminal_handshake_error(&kind) {
            ReconnectDetachOutcome::Fatal(HandshakeFailure::new(kind, BufferedEvents::default()))
        } else {
            ReconnectDetachOutcome::Retry(BufferedEvents::default())
        };
    }
    match read_reply_handshake(reader, request_id, timeout, cancel_token).await {
        Ok((Reply::Ok, events)) => ReconnectDetachOutcome::Detached(events),
        Ok((Reply::Error { .. }, events)) => ReconnectDetachOutcome::Retry(events),
        Ok((_, events)) => {
            ReconnectDetachOutcome::Fatal(HandshakeFailure::new(HandshakeError::Unexpected, events))
        }
        Err(failure) => {
            if is_terminal_handshake_error(&failure.kind) {
                ReconnectDetachOutcome::Fatal(failure)
            } else {
                ReconnectDetachOutcome::Retry(failure.events)
            }
        }
    }
}

pub(crate) struct SupervisorCtx {
    pub reader: BufReader<OwnedReadHalf>,
    pub socket_path: PathBuf,
    pub client_id: ClientId,
    pub opts: ClientOptions,
    pub next_id: Arc<AtomicU64>,
    pub pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pub pending_attaches: Arc<Mutex<HashMap<u64, AttachAttempt>>>,
    pub events_tx: EventSink,
    pub dropped_events: Arc<AtomicUsize>,
    pub writer_tx_holder: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pub connected: Arc<AtomicBool>,
    pub writer: Option<WriterGeneration>,
    pub lifecycle: Arc<LifecycleState>,
    pub cancel_token: CancellationToken,
    /// The last `Subscribe` the caller sent; replayed on every new connection.
    pub interest: super::DesiredInterest,
}

async fn stop_transport(ctx: &mut SupervisorCtx) {
    ctx.connected.store(false, Ordering::Relaxed);
    *lock(&ctx.writer_tx_holder) = None;
    fail_pending(&ctx.pending, &ctx.pending_attaches);
    if let Some(writer) = ctx.writer.take() {
        writer.shutdown().await;
    }
}

pub(crate) async fn run_supervisor(mut ctx: SupervisorCtx) {
    run_supervisor_loop(&mut ctx).await;
    stop_transport(&mut ctx).await;
}

async fn run_supervisor_loop(ctx: &mut SupervisorCtx) {
    loop {
        if ctx.cancel_token.is_cancelled() {
            break;
        }
        if ctx.events_tx.is_closed() {
            stop_transport(ctx).await;
            break;
        }
        let Some(writer) = ctx.writer.as_mut() else {
            let context = ctx.lifecycle.context();
            stop_transport(ctx).await;
            ctx.events_tx.terminate(ClientEvent::Disconnected {
                context,
                reason: "writer unavailable".into(),
            });
            return;
        };
        let reason = run_reader(
            &mut ctx.reader,
            &ctx.pending,
            &ctx.pending_attaches,
            &ctx.events_tx,
            &ctx.lifecycle,
            writer.status(),
            &ctx.cancel_token,
        )
        .await;
        let reason = match reason {
            ReaderExit::Cancelled => break,
            ReaderExit::Terminal(reason) => {
                let context = ctx.lifecycle.context();
                stop_transport(ctx).await;
                ctx.events_tx
                    .terminate(ClientEvent::Disconnected { context, reason });
                return;
            }
            ReaderExit::Retryable(reason) => reason,
        };
        if ctx.cancel_token.is_cancelled() {
            break;
        }
        if ctx.events_tx.is_closed() {
            stop_transport(ctx).await;
            break;
        }

        log::warn!("daemon connection lost: {reason}");
        stop_transport(ctx).await;
        emit_event(
            &ctx.events_tx,
            &ctx.dropped_events,
            ClientEvent::Disconnected {
                context: ctx.lifecycle.context(),
                reason,
            },
        )
        .await;
        if ctx.events_tx.is_closed() {
            stop_transport(ctx).await;
            return;
        }

        let mut reconnect_attempt = 1u32;
        let mut backoff = std::time::Duration::from_millis(500);
        let mut pending_events = BufferedEvents::default();

        let next_reader = loop {
            if ctx.cancel_token.is_cancelled() {
                return;
            }
            if ctx.events_tx.is_closed() {
                stop_transport(ctx).await;
                return;
            }
            emit_event(
                &ctx.events_tx,
                &ctx.dropped_events,
                ClientEvent::Reconnecting {
                    context: ctx.lifecycle.context(),
                    attempt: reconnect_attempt,
                },
            )
            .await;
            if ctx.events_tx.is_closed() {
                stop_transport(ctx).await;
                return;
            }
            reconnect_attempt = reconnect_attempt.saturating_add(1);
            let delay = backoff;
            backoff = next_backoff(backoff);
            tokio::select! {
                _ = ctx.cancel_token.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
            if ctx.cancel_token.is_cancelled() {
                return;
            }
            let stream = tokio::select! {
                biased;
                _ = ctx.cancel_token.cancelled() => return,
                result = tokio::net::UnixStream::connect(&ctx.socket_path) => result,
            };
            let stream = match stream {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let (read_half, mut write_half) = stream.into_split();
            let mut current_reader = BufReader::new(read_half);
            let hello = match do_reconnect_hello(
                &mut write_half,
                &mut current_reader,
                &ctx.client_id,
                &ctx.opts.app_version,
                ctx.opts.request_timeout,
                &ctx.cancel_token,
            )
            .await
            {
                Ok(hello) => hello,
                Err(failure) => {
                    if is_terminal_handshake_error(&failure.kind) {
                        let reason = client_error(failure.kind).to_string();
                        stop_transport(ctx).await;
                        ctx.events_tx.terminate(ClientEvent::Disconnected {
                            context: ctx.lifecycle.context(),
                            reason,
                        });
                        return;
                    }
                    pending_events.merge(failure.events);
                    continue;
                }
            };
            let (daemon_version, pid, version_mismatch, events) = hello;
            pending_events.merge(events);
            let subscribe = lock(&ctx.interest)
                .clone()
                .map(|(agents, all)| Request::Subscribe { agents, all });
            if let Some(subscribe) = subscribe {
                match do_reconnect_subscribe(
                    &mut write_half,
                    &mut current_reader,
                    ctx.next_id.fetch_add(1, Ordering::Relaxed),
                    subscribe,
                    ctx.opts.request_timeout,
                    &ctx.cancel_token,
                )
                .await
                {
                    Ok(events) => pending_events.merge(events),
                    Err(failure) => {
                        if is_terminal_handshake_error(&failure.kind) {
                            let reason = client_error(failure.kind).to_string();
                            stop_transport(ctx).await;
                            ctx.events_tx.terminate(ClientEvent::Disconnected {
                                context: ctx.lifecycle.context(),
                                reason,
                            });
                            return;
                        }
                        pending_events.merge(failure.events);
                        continue;
                    }
                }
            }
            if pending_events.overflowed {
                ctx.events_tx.terminate(ClientEvent::Disconnected {
                    context: ctx.lifecycle.context(),
                    reason: "client handshake event buffer overflow".into(),
                });
                return;
            }
            let connection_epoch = ctx.lifecycle.advance_connection();
            let desired = ctx
                .lifecycle
                .renew_attach_request_id(ctx.next_id.fetch_add(1, Ordering::Relaxed));
            let attach_result = if let Some(attempt) = desired {
                reconnect_attach_until_desired(
                    AttachRetryCtx {
                        write_half: &mut write_half,
                        reader: &mut current_reader,
                        lifecycle: &ctx.lifecycle,
                        pending_events: &mut pending_events,
                        next_id: ctx.next_id.as_ref(),
                        timeout: ctx.opts.request_timeout,
                        cancel_token: &ctx.cancel_token,
                    },
                    attempt,
                    connection_epoch,
                )
                .await
            } else {
                ReconnectAttachResult::Detached
            };
            let (accepted_attempt, snapshot, snapshot_observation) = match attach_result {
                ReconnectAttachResult::Attached {
                    attempt,
                    snapshot,
                    observation,
                } => (Some(attempt), Some(snapshot), Some(observation)),
                ReconnectAttachResult::Detached => (None, None, None),
                ReconnectAttachResult::Retry => {
                    if pending_events.overflowed {
                        ctx.events_tx.terminate(ClientEvent::Disconnected {
                            context: ctx.lifecycle.context(),
                            reason: "client handshake event buffer overflow".into(),
                        });
                        return;
                    }
                    continue;
                }
                ReconnectAttachResult::Fatal { reason } => {
                    ctx.events_tx.terminate(ClientEvent::Disconnected {
                        context: ctx.lifecycle.context(),
                        reason,
                    });
                    return;
                }
                ReconnectAttachResult::Error {
                    attempt,
                    target,
                    reason,
                } => {
                    if pending_events.overflowed {
                        ctx.events_tx.terminate(ClientEvent::Disconnected {
                            context: ctx.lifecycle.context(),
                            reason: "client handshake event buffer overflow".into(),
                        });
                        return;
                    }
                    let Some(context) = ctx.lifecycle.clear_failed_attach(&attempt) else {
                        continue;
                    };
                    log::error!("reconnect attach failed: {reason}");
                    emit_event(
                        &ctx.events_tx,
                        &ctx.dropped_events,
                        ClientEvent::AttachFailed {
                            context,
                            attach: target,
                            reason,
                        },
                    )
                    .await;
                    continue;
                }
            };
            if pending_events.overflowed {
                ctx.events_tx.terminate(ClientEvent::Disconnected {
                    context: ctx.lifecycle.context(),
                    reason: "client handshake event buffer overflow".into(),
                });
                return;
            }
            let snapshot_id = snapshot.as_ref().map(|snapshot| snapshot.meta.id.clone());
            let writer_tx_holder = ctx.writer_tx_holder.clone();
            let connected = ctx.connected.clone();
            let writer_cancel = ctx.cancel_token.clone();
            let write_timeout = ctx.opts.request_timeout;
            let mut write_half = Some(write_half);
            let mut prepared_writer = None;
            let reconnect_context = ctx.lifecycle.commit_connection(
                accepted_attempt.as_ref(),
                snapshot_id.as_ref(),
                || {
                    if let Some(half) = write_half.take() {
                        let writer = spawn_writer(half, writer_cancel, write_timeout);
                        *lock(&writer_tx_holder) = Some(writer.sender());
                        connected.store(true, Ordering::Relaxed);
                        prepared_writer = Some(writer);
                    }
                },
            );
            let Some(reconnect_context) = reconnect_context else {
                let Some(mut write_half) = write_half else {
                    stop_transport(ctx).await;
                    ctx.events_tx.terminate(ClientEvent::Disconnected {
                        context: ctx.lifecycle.context(),
                        reason: "connection commit failed".into(),
                    });
                    return;
                };
                let detach_id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
                match do_reconnect_detach(
                    &mut write_half,
                    &mut current_reader,
                    detach_id,
                    ctx.opts.request_timeout,
                    &ctx.cancel_token,
                )
                .await
                {
                    ReconnectDetachOutcome::Detached(events)
                    | ReconnectDetachOutcome::Retry(events) => {
                        pending_events.merge(events);
                        if pending_events.overflowed {
                            ctx.events_tx.terminate(ClientEvent::Disconnected {
                                context: ctx.lifecycle.context(),
                                reason: "client handshake event buffer overflow".into(),
                            });
                            return;
                        }
                    }
                    ReconnectDetachOutcome::Fatal(failure) => {
                        let reason = client_error(failure.kind).to_string();
                        stop_transport(ctx).await;
                        ctx.events_tx.terminate(ClientEvent::Disconnected {
                            context: ctx.lifecycle.context(),
                            reason,
                        });
                        return;
                    }
                }
                continue;
            };
            let Some(writer) = prepared_writer else {
                stop_transport(ctx).await;
                ctx.events_tx.terminate(ClientEvent::Disconnected {
                    context: ctx.lifecycle.context(),
                    reason: "connection commit failed".into(),
                });
                return;
            };
            ctx.writer = Some(writer);
            let (buffered_events, resync_needed, resync_seq, _) =
                std::mem::take(&mut pending_events).into_parts();
            log::info!("reconnected to daemon: pid={pid}, daemon_version={daemon_version}");
            emit_event(
                &ctx.events_tx,
                &ctx.dropped_events,
                ClientEvent::Connected {
                    context: reconnect_context.clone(),
                    daemon_version,
                    pid,
                    version_mismatch,
                },
            )
            .await;
            if ctx.events_tx.is_closed() {
                stop_transport(ctx).await;
                return;
            }
            for event in buffered_events {
                let BufferedServerEvent::Event { seq, event } = event else {
                    continue;
                };
                if let Some(context) = ctx.lifecycle.accepted_event_context(seq, event.as_ref()) {
                    emit_event(
                        &ctx.events_tx,
                        &ctx.dropped_events,
                        ClientEvent::Event {
                            context,
                            seq,
                            event: *event,
                        },
                    )
                    .await;
                }
            }
            if let Some(snapshot) = snapshot {
                if snapshot_observation.is_some_and(|observation| observation.monotonic) {
                    ctx.events_tx
                        .snapshot_applied(&snapshot.meta.id, snapshot.seq);
                }
                emit_event(
                    &ctx.events_tx,
                    &ctx.dropped_events,
                    ClientEvent::Snapshot {
                        context: reconnect_context.clone(),
                        snapshot: *snapshot,
                    },
                )
                .await;
            }
            if resync_needed
                && let Some(context) = ctx.lifecycle.accepted_resync_context(resync_seq)
            {
                ctx.events_tx.emit_resync(context, resync_seq);
            }
            break current_reader;
        };
        ctx.reader = next_reader;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use pacode_types::ServerMessage;
    use tokio::io::{AsyncWrite, BufReader};
    use tokio::net::UnixStream;
    use tokio::sync::{mpsc, oneshot, watch};

    use crate::{EventSink, LifecycleState};

    use super::{
        HandshakeError, ReaderExit, WriterStatus, is_terminal_handshake_error, run_reader,
        run_writer,
    };

    #[derive(Clone, Copy)]
    enum StallStage {
        Write,
        Flush,
    }

    struct StalledWriter {
        stage: StallStage,
        started: Option<oneshot::Sender<()>>,
    }

    impl StalledWriter {
        fn new(stage: StallStage) -> (Self, oneshot::Receiver<()>) {
            let (started_tx, started_rx) = oneshot::channel();
            (
                Self {
                    stage,
                    started: Some(started_tx),
                },
                started_rx,
            )
        }

        fn signal_started(&mut self) {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
        }
    }

    impl AsyncWrite for StalledWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            if matches!(self.stage, StallStage::Write) {
                self.signal_started();
                return Poll::Pending;
            }
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<io::Result<()>> {
            if matches!(self.stage, StallStage::Flush) {
                self.signal_started();
                return Poll::Pending;
            }
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn malformed_and_rejected_reconnect_handshakes_are_terminal() {
        let json = serde_json::from_str::<ServerMessage>("not-json").unwrap_err();
        assert!(is_terminal_handshake_error(&HandshakeError::Json(json)));
        assert!(is_terminal_handshake_error(&HandshakeError::Daemon(
            "rejected".into()
        )));
        assert!(is_terminal_handshake_error(&HandshakeError::Unexpected));
        assert!(!is_terminal_handshake_error(&HandshakeError::Timeout));
        assert!(!is_terminal_handshake_error(&HandshakeError::Closed));
    }

    #[tokio::test]
    async fn writer_failure_wakes_reader_without_lost_notification() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let (read_half, _write_half) = stream.into_split();
        let cancel = tokio_util::sync::CancellationToken::new();
        let (events_tx, _events_rx) = EventSink::channel(cancel.clone());
        let lifecycle = LifecycleState::new(1);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_attaches = Arc::new(Mutex::new(HashMap::new()));
        let (failed, mut status) = watch::channel(WriterStatus::Running);
        failed.send(WriterStatus::Failed).unwrap();

        let exit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_reader(
                &mut BufReader::new(read_half),
                &pending,
                &pending_attaches,
                &events_tx,
                &lifecycle,
                &mut status,
                &cancel,
            ),
        )
        .await
        .unwrap();
        assert!(matches!(exit, ReaderExit::Retryable(reason) if reason == "writer failed"));
    }

    #[tokio::test]
    async fn stalled_writer_failure_is_retained_for_supervisor() {
        let (failed, mut status) = watch::channel(WriterStatus::Running);
        let (sender, receiver) = mpsc::channel(1);
        let (writer, started) = StalledWriter::new(StallStage::Write);
        let task = tokio::spawn(run_writer(
            writer,
            receiver,
            tokio_util::sync::CancellationToken::new(),
            failed,
            std::time::Duration::from_millis(10),
        ));
        sender.send("line\n".into()).await.unwrap();
        started.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*status.borrow(), WriterStatus::Failed);
        tokio::time::timeout(std::time::Duration::from_millis(50), status.changed())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stalled_write_is_cancelled() {
        assert_stalled_io_is_cancelled(StallStage::Write).await;
    }

    #[tokio::test]
    async fn stalled_flush_is_cancelled() {
        assert_stalled_io_is_cancelled(StallStage::Flush).await;
    }

    #[tokio::test]
    async fn late_old_writer_failure_does_not_signal_new_generation() {
        let (old_failed, mut old_status) = watch::channel(WriterStatus::Running);
        let (sender, receiver) = mpsc::channel(1);
        let (writer, started) = StalledWriter::new(StallStage::Write);
        let old_task = tokio::spawn(run_writer(
            writer,
            receiver,
            tokio_util::sync::CancellationToken::new(),
            old_failed,
            std::time::Duration::from_millis(10),
        ));
        sender.send("line\n".into()).await.unwrap();
        started.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), old_task)
            .await
            .unwrap()
            .unwrap();
        old_status.changed().await.unwrap();

        let (_new_failed, mut new_status) = watch::channel(WriterStatus::Running);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), new_status.changed())
                .await
                .is_err()
        );
    }

    async fn assert_stalled_io_is_cancelled(stage: StallStage) {
        let (failed, _status) = watch::channel(WriterStatus::Running);
        let (sender, receiver) = mpsc::channel(1);
        let (writer, started) = StalledWriter::new(stage);
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(run_writer(
            writer,
            receiver,
            cancel.clone(),
            failed,
            std::time::Duration::from_secs(60),
        ));
        sender.send("line\n".into()).await.unwrap();
        started.await.unwrap();
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
