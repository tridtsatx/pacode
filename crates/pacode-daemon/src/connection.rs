//! One client connection.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacode_core::core::SessionLease;
use pacode_core::{Core, CoreError};
use pacode_store::Store;
use pacode_store::queries::{AttachAdmission, AttachAdmissionResult, SessionFilter};
use pacode_types::ids::ClientId;
use pacode_types::protocol::{
    AttachEnvelope, Event, PROTOCOL_VERSION, Reply, Request, ServerMessage,
};
use pacode_types::{AgentId, Attach, SessionId};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

/// Shared control handle: shutdown request + connection counter.
#[derive(Clone)]
pub struct ServerControl {
    pub shutdown: tokio_util::sync::CancellationToken,
    /// One-shot "exit when the core goes idle" request; `Shutdown{force:false}`.
    pub graceful_shutdown: tokio_util::sync::CancellationToken,
    pub connections: Arc<std::sync::atomic::AtomicUsize>,
    pub app_version: String,
    pub pid: u32,
    pub paths: pacode_config::Paths,
    pub disconnected: Arc<tokio::sync::Notify>,
    pub attach_store: Store,
}

struct ConnectionGuard {
    connections: Arc<std::sync::atomic::AtomicUsize>,
    disconnected: Arc<tokio::sync::Notify>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.disconnected.notify_one();
    }
}

/// Fire-and-forget MCP status refresh. `ListMcpServers` walks the pool —
/// listing prompts of a ready server can start a lazy one — so it always
/// runs detached; a client reply must never wait on it.
fn spawn_mcp_status_refresh(core: &Arc<Core>, tasks: &mut JoinSet<()>) {
    let core = Arc::clone(core);
    tasks.spawn(async move {
        let _ = core.handle_global(&Request::ListMcpServers).await;
    });
}

const MAX_IN_FLIGHT_REQUESTS: usize = 8;
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_CONTROL_BURST: usize = 8;
const MAX_NDJSON_LINE: usize = 8 * 1024 * 1024;
const MAX_OUTBOUND_LINE: usize = MAX_NDJSON_LINE;
const WIRE_BYTE_BUDGET: usize = 8 * 1024 * 1024;
const WIRE_PERMIT_GRANULARITY: usize = 512;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WIRE_QUEUE_CAP: usize = 1024;
/// Soft cap on the data lines the writer joins into one write; one longer line is
/// still written whole. Nothing waits for a batch to fill.
const WIRE_WRITE_BATCH: usize = 64 * 1024;
const ATTACH_LEASE_ATTEMPTS: usize = 3;

fn ndjson(line: String) -> String {
    let mut line = line;
    line.push('\n');
    line
}

fn wire_permits(bytes: usize) -> u32 {
    let permits = bytes.div_ceil(WIRE_PERMIT_GRANULARITY).max(1);
    u32::try_from(permits).unwrap_or(u32::MAX)
}

fn outbound_reply_line(id: u64, reply: Reply) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ServerMessage::Reply { id, reply })
}

struct RequestJob {
    id: u64,
    request: Request,
    session: Option<SessionId>,
    generation: u64,
}

struct RequestResult {
    id: u64,
    generation: u64,
    session_bound: bool,
    reply: Option<Reply>,
}

enum ConnectionRequest {
    Hello,
    Attach(Attach),
    Detach,
    CloseSession,
    Shutdown {
        force: bool,
    },
    Ping,
    /// Narrow the high-volume session events this connection receives.
    Subscribe {
        agents: Vec<AgentId>,
        all: bool,
    },
    /// Daemon counters for benchmarks and diagnostics.
    DebugStats,
}

fn into_connection_request(request: Request) -> Result<ConnectionRequest, Request> {
    match request {
        Request::Hello(_) => Ok(ConnectionRequest::Hello),
        Request::Attach(attach) => Ok(ConnectionRequest::Attach(attach)),
        Request::Detach => Ok(ConnectionRequest::Detach),
        Request::CloseSession => Ok(ConnectionRequest::CloseSession),
        Request::Shutdown { force } => Ok(ConnectionRequest::Shutdown { force }),
        Request::Ping => Ok(ConnectionRequest::Ping),
        Request::Subscribe { agents, all } => Ok(ConnectionRequest::Subscribe { agents, all }),
        Request::DebugStats => Ok(ConnectionRequest::DebugStats),
        request => Err(request),
    }
}

fn cancelled_result(id: u64, generation: u64, session_bound: bool) -> RequestResult {
    RequestResult {
        id,
        generation,
        session_bound,
        reply: None,
    }
}

fn attachment_lease_held(
    generation: &AtomicU64,
    lease: &CancellationToken,
    shutdown: &CancellationToken,
    session_bound: bool,
    job_generation: u64,
) -> bool {
    !session_bound
        || (!lease.is_cancelled()
            && generation.load(Ordering::Acquire) == job_generation
            && !shutdown.is_cancelled())
}

async fn execute_request(
    core: Arc<Core>,
    job: RequestJob,
    attachment_generation: Arc<AtomicU64>,
    attachment_cancel: CancellationToken,
    shutdown: CancellationToken,
) -> RequestResult {
    let session_bound = job.session.is_some();
    if !attachment_lease_held(
        &attachment_generation,
        &attachment_cancel,
        &shutdown,
        session_bound,
        job.generation,
    ) {
        return cancelled_result(job.id, job.generation, session_bound);
    }

    let RequestJob {
        id,
        request,
        session,
        generation,
    } = job;
    let reply = match session {
        Some(session) => {
            tokio::select! {
                biased;
                _ = attachment_cancel.cancelled() => {
                    return cancelled_result(id, generation, true);
                }
                _ = shutdown.cancelled() => {
                    return cancelled_result(id, generation, true);
                }
                reply = core.handle(&session, request) => reply,
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    return cancelled_result(id, generation, false);
                }
                reply = core.handle_global(&request) => reply.unwrap_or(Reply::Error {
                    message: "not attached to a session".to_string(),
                }),
            }
        }
    };
    let stale = session_bound
        && (attachment_cancel.is_cancelled()
            || attachment_generation.load(Ordering::Acquire) != generation);
    RequestResult {
        id,
        generation,
        session_bound,
        reply: (!stale).then_some(reply),
    }
}

fn spawn_request(
    core: &Arc<Core>,
    job: RequestJob,
    tasks: &mut JoinSet<RequestResult>,
    attachment_generation: &Arc<AtomicU64>,
    attachment_cancel: &CancellationToken,
    shutdown: &CancellationToken,
) {
    let core = Arc::clone(core);
    let attachment_generation = Arc::clone(attachment_generation);
    let attachment_cancel = attachment_cancel.clone();
    let shutdown = shutdown.clone();
    tasks.spawn(execute_request(
        core,
        job,
        attachment_generation,
        attachment_cancel,
        shutdown,
    ));
}

enum InputLineError {
    Closed,
    Overflow,
    Io,
}

enum HandshakeRead {
    Line,
    Closed,
    Overflow,
    TimedOut,
    Cancelled,
    WriterStopped,
}

async fn read_input_line<R>(reader: &mut R, line: &mut Vec<u8>) -> Result<(), InputLineError>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    loop {
        let (consumed, done, overflow) = {
            let available = reader.fill_buf().await.map_err(|_| InputLineError::Io)?;
            if available.is_empty() {
                return if line.is_empty() {
                    Err(InputLineError::Closed)
                } else {
                    Ok(())
                };
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
            return Err(InputLineError::Overflow);
        }
        if done {
            return Ok(());
        }
    }
}

fn line_overflow_message() -> String {
    format!("NDJSON line too long: exceeds {MAX_NDJSON_LINE} bytes")
}

async fn read_handshake_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    shutdown: &CancellationToken,
    writer_finished: &CancellationToken,
) -> HandshakeRead
where
    R: AsyncBufRead + Unpin,
{
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => HandshakeRead::Cancelled,
        _ = writer_finished.cancelled() => HandshakeRead::WriterStopped,
        result = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_input_line(reader, line)) => {
            match result {
                Ok(Ok(())) => HandshakeRead::Line,
                Ok(Err(InputLineError::Overflow)) => HandshakeRead::Overflow,
                Ok(Err(InputLineError::Closed | InputLineError::Io)) => HandshakeRead::Closed,
                Err(_) => HandshakeRead::TimedOut,
            }
        }
    }
}

type ResyncState = Arc<Mutex<Option<Option<u64>>>>;

#[derive(Clone)]
struct ResyncControl {
    tx: mpsc::Sender<()>,
    state: ResyncState,
}

impl ResyncControl {
    fn new() -> (Self, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel(1);
        (
            Self {
                tx,
                state: Arc::new(Mutex::new(None)),
            },
            rx,
        )
    }

    fn signal(&self, current_seq: Option<u64>) {
        let mut pending = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = Some(match (*pending, current_seq) {
            (None, next) => next,
            (Some(None), _) => None,
            (Some(Some(previous)), Some(next)) => Some(previous.max(next)),
            (Some(Some(_)), None) => None,
        });
        drop(pending);
        let _ = self.tx.try_send(());
    }

    fn take(&self) -> Option<Option<u64>> {
        let mut pending = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.take()
    }
}

struct OutboundLine {
    text: String,
    written: Option<oneshot::Sender<bool>>,
    permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct WireSender {
    tx: mpsc::Sender<OutboundLine>,
    resync: ResyncControl,
    budget: Arc<Semaphore>,
    shutdown: CancellationToken,
    lifecycle: CancellationToken,
}

impl WireSender {
    async fn reserve_outbound(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        if self.shutdown.is_cancelled() || self.lifecycle.is_cancelled() {
            return None;
        }
        let budget = Arc::clone(&self.budget);
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => None,
            _ = self.lifecycle.cancelled() => None,
            permit = budget.acquire_many_owned(wire_permits(bytes)) => permit.ok(),
        }
    }

    /// Outbound budget for the last line of a connection. Only `lifecycle` may
    /// refuse it: a shutdown reply is enqueued *after* the shutdown was
    /// requested, so gating it on `shutdown` would drop exactly the line the
    /// client is waiting for.
    async fn reserve_outbound_final(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        if self.lifecycle.is_cancelled() {
            return None;
        }
        let budget = Arc::clone(&self.budget);
        tokio::select! {
            biased;
            _ = self.lifecycle.cancelled() => None,
            permit = budget.acquire_many_owned(wire_permits(bytes)) => permit.ok(),
        }
    }

    fn try_reserve_outbound(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        self.budget
            .clone()
            .try_acquire_many_owned(wire_permits(bytes))
            .ok()
    }

    fn drop_event(
        &self,
        seq: u64,
        bytes: usize,
        dropped_events: &AtomicUsize,
        reason: &str,
    ) -> bool {
        let count = dropped_events.fetch_add(1, Ordering::Relaxed) + 1;
        log::warn!("{reason}, dropped seq={seq} bytes={bytes} dropped_events={count}");
        self.request_resync(Some(seq));
        true
    }

    async fn send_line(&self, text: String) -> bool {
        let Some(permit) = self.reserve_outbound(text.len()).await else {
            return false;
        };
        let (written_tx, written_rx) = oneshot::channel();
        let outbound = OutboundLine {
            text,
            written: Some(written_tx),
            permit,
        };
        let sent = tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => return false,
            _ = self.lifecycle.cancelled() => return false,
            result = self.tx.send(outbound) => result.is_ok(),
        };
        if !sent {
            return false;
        }
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => false,
            _ = self.lifecycle.cancelled() => false,
            written = written_rx => written.unwrap_or(false),
        }
    }

    /// Enqueue a line that must reach the client even when a shutdown is already
    /// in flight, and wait for it to be flushed. `lifecycle` is the only abort
    /// left: it is cancelled by `stop_wire_writer`, which only runs after this
    /// call returned. The writer task completes an in-progress write regardless
    /// of `shutdown` and drains what is already queued, so "reply then close"
    /// holds even when the server's shutdown token fires mid-flush.
    async fn send_line_final(&self, text: String) -> bool {
        let Some(permit) = self.reserve_outbound_final(text.len()).await else {
            return false;
        };
        let (written_tx, written_rx) = oneshot::channel();
        let outbound = OutboundLine {
            text,
            written: Some(written_tx),
            permit,
        };
        if self.tx.send(outbound).await.is_err() {
            return false;
        }
        tokio::select! {
            biased;
            _ = self.lifecycle.cancelled() => false,
            written = written_rx => written.unwrap_or(false),
        }
    }

    async fn send_reply(&self, id: u64, reply: Reply) -> bool {
        let line = match outbound_reply_line(id, reply) {
            Ok(line) => line,
            Err(error) => {
                log::error!("failed to serialize ServerMessage: {error}");
                return false;
            }
        };
        if line.len() > MAX_OUTBOUND_LINE {
            log::warn!(
                "outbound reply {id} of {} bytes exceeds {MAX_OUTBOUND_LINE} bytes",
                line.len()
            );
            let message = format!(
                "reply of {} bytes exceeds the {MAX_OUTBOUND_LINE} byte wire limit",
                line.len()
            );
            return match outbound_reply_line(id, Reply::Error { message }) {
                Ok(line) => self.send_line(ndjson(line)).await,
                Err(error) => {
                    log::error!("failed to serialize ServerMessage: {error}");
                    false
                }
            };
        }
        self.send_line(ndjson(line)).await
    }

    /// `send_reply` for the last line of a connection: survives a shutdown token
    /// that is already cancelled.
    async fn send_reply_final(&self, id: u64, reply: Reply) -> bool {
        let line = match outbound_reply_line(id, reply) {
            Ok(line) => line,
            Err(error) => {
                log::error!("failed to serialize ServerMessage: {error}");
                return false;
            }
        };
        if line.len() > MAX_OUTBOUND_LINE {
            log::warn!(
                "outbound reply {id} of {} bytes exceeds {MAX_OUTBOUND_LINE} bytes",
                line.len()
            );
            let message = format!(
                "reply of {} bytes exceeds the {MAX_OUTBOUND_LINE} byte wire limit",
                line.len()
            );
            return match outbound_reply_line(id, Reply::Error { message }) {
                Ok(line) => self.send_line_final(ndjson(line)).await,
                Err(error) => {
                    log::error!("failed to serialize ServerMessage: {error}");
                    false
                }
            };
        }
        self.send_line_final(ndjson(line)).await
    }

    fn request_resync(&self, current_seq: Option<u64>) {
        self.resync.signal(current_seq);
    }

    async fn send_event(&self, seq: u64, event: Event, dropped_events: &AtomicUsize) -> bool {
        let lossy = event.is_lossy_delta();
        let line = match serde_json::to_string(&ServerMessage::Event { seq, event }) {
            Ok(line) => line,
            Err(error) => {
                log::error!("failed to serialize ServerMessage: {error}");
                return false;
            }
        };
        if line.len() > MAX_OUTBOUND_LINE {
            return self.drop_event(
                seq,
                line.len(),
                dropped_events,
                "outbound event exceeds the wire line limit",
            );
        }
        if lossy {
            let bytes = line.len() + 1;
            let Some(permit) = self.try_reserve_outbound(bytes) else {
                return self.drop_event(
                    seq,
                    bytes,
                    dropped_events,
                    "outbound wire budget exhausted",
                );
            };
            return match self.tx.try_send(OutboundLine {
                text: ndjson(line),
                written: None,
                permit,
            }) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.drop_event(seq, bytes, dropped_events, "connection event queue full")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            };
        }
        self.send_line(ndjson(line)).await
    }
}

enum WriterWake {
    Cancelled,
    Control(Option<()>),
    Data(Option<OutboundLine>),
}

async fn write_wire_line<W>(writer: &mut W, line: &str, lifecycle: &CancellationToken) -> bool
where
    W: AsyncWrite + Unpin,
{
    let write = async {
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await
    };
    tokio::pin!(write);
    tokio::select! {
        biased;
        _ = lifecycle.cancelled() => false,
        result = &mut write => result.is_ok(),
    }
}

struct WriterFinished(CancellationToken);

impl Drop for WriterFinished {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn spawn_wire_writer<W>(
    mut writer: W,
    mut data_rx: mpsc::Receiver<OutboundLine>,
    resync: ResyncControl,
    mut control_rx: mpsc::Receiver<()>,
    lifecycle: CancellationToken,
    finished: CancellationToken,
) -> JoinHandle<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    // The writer deliberately takes no server-wide shutdown token. The final
    // line of a connection is enqueued *after* that shutdown was requested, so
    // exiting on it would drop the very reply the client is blocked on.
    // Teardown is owned by the connection through `lifecycle`, which
    // `stop_wire_writer` cancels on every exit path.
    tokio::spawn(async move {
        let _finished = WriterFinished(finished);
        let mut batch: Vec<OutboundLine> = Vec::new();
        let mut joined = String::new();
        let mut data_open = true;
        let mut control_open = true;
        let mut control_streak = 0;
        while data_open || control_open {
            let prefer_control = control_streak < MAX_CONTROL_BURST;
            let wake = if prefer_control {
                tokio::select! {
                    biased;
                    _ = lifecycle.cancelled() => WriterWake::Cancelled,
                    control = control_rx.recv(), if control_open => WriterWake::Control(control),
                    data = data_rx.recv(), if data_open => WriterWake::Data(data),
                }
            } else {
                tokio::select! {
                    biased;
                    _ = lifecycle.cancelled() => WriterWake::Cancelled,
                    data = data_rx.recv(), if data_open => WriterWake::Data(data),
                    control = control_rx.recv(), if control_open => WriterWake::Control(control),
                }
            };
            match wake {
                WriterWake::Cancelled => break,
                WriterWake::Control(Some(())) => {
                    let Some(current_seq) = resync.take() else {
                        continue;
                    };
                    let mut line =
                        match serde_json::to_string(&ServerMessage::Resync { current_seq }) {
                            Ok(line) => line,
                            Err(error) => {
                                log::error!("failed to serialize ServerMessage: {error}");
                                break;
                            }
                        };
                    line.push('\n');
                    if !write_wire_line(&mut writer, &line, &lifecycle).await {
                        break;
                    }
                    control_streak += 1;
                }
                WriterWake::Control(None) => control_open = false,
                WriterWake::Data(Some(outbound)) => {
                    // Everything already queued goes out in one write and one flush;
                    // each line's `written` ack fires only after that flush, and its
                    // budget permit is released only then.
                    batch.push(outbound);
                    let mut bytes = batch[0].text.len();
                    while bytes < WIRE_WRITE_BATCH {
                        let Ok(next) = data_rx.try_recv() else {
                            break;
                        };
                        bytes += next.text.len();
                        batch.push(next);
                    }
                    let delivered = if batch.len() == 1 {
                        write_wire_line(&mut writer, &batch[0].text, &lifecycle).await
                    } else {
                        joined.clear();
                        for line in &batch {
                            joined.push_str(&line.text);
                        }
                        write_wire_line(&mut writer, &joined, &lifecycle).await
                    };
                    if joined.capacity() > 4 * WIRE_WRITE_BATCH {
                        joined = String::new();
                    }
                    for line in batch.drain(..) {
                        drop(line.permit);
                        if let Some(written_tx) = line.written {
                            let _ = written_tx.send(delivered);
                        }
                    }
                    control_streak = 0;
                    if !delivered {
                        break;
                    }
                }
                WriterWake::Data(None) => data_open = false,
            }
        }
    })
}

async fn stop_wire_writer(wire: WireSender, writer: JoinHandle<()>) {
    wire.lifecycle.cancel();
    drop(wire);
    let _ = writer.await;
}

fn authoritative_global_events(core: &Core) -> Vec<Event> {
    let mut events = Vec::new();
    if let Reply::AuthStatus { providers } = core.handle_list_auth() {
        events.extend(providers.into_iter().map(Event::AuthUpdated));
    }
    events.extend(core.session_ids().into_iter().filter_map(|session_id| {
        let session = core.session(&session_id)?;
        let needs_input =
            !session.permissions.pending().is_empty() || !session.questions.pending().is_empty();
        let running = session
            .agents
            .read()
            .map(|agents| agents.values().any(|agent| agent.is_running()))
            .unwrap_or_else(|poisoned| {
                poisoned
                    .into_inner()
                    .values()
                    .any(|agent| agent.is_running())
            });
        let status = if needs_input {
            pacode_types::SessionStatusKind::NeedsInput
        } else if running {
            pacode_types::SessionStatusKind::Working
        } else if session.meta().first_prompt.is_some() {
            pacode_types::SessionStatusKind::Done
        } else {
            pacode_types::SessionStatusKind::Idle
        };
        Some(Event::SessionStatus {
            session: session_id,
            status,
        })
    }));
    events
}

async fn recover_global<F>(
    receiver: &mut broadcast::Receiver<(u64, Event)>,
    wire: &WireSender,
    dropped_events: &AtomicUsize,
    last_seq: &mut u64,
    mut recovery_events: F,
) -> bool
where
    F: FnMut() -> Vec<Event>,
{
    loop {
        match receiver.try_recv() {
            Ok((seq, event)) => {
                *last_seq = seq;
                if !wire.send_event(seq, event, dropped_events).await {
                    return false;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(count)) => {
                log::warn!("global event receiver remained behind by {count} events");
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => return false,
        }
    }
    for event in recovery_events() {
        *last_seq = last_seq.saturating_add(1);
        if !wire.send_event(*last_seq, event, dropped_events).await {
            return false;
        }
    }
    wire.request_resync(None);
    true
}

const SESSION_FORWARD_EVENT_CAP: usize = 1024;

enum QueueEventResult {
    Queued,
    Full,
    Closed,
}

fn queue_event(
    event_tx: &mpsc::Sender<(u64, Event)>,
    control: &ResyncControl,
    seq: u64,
    event: Event,
) -> QueueEventResult {
    match event_tx.try_send((seq, event)) {
        Ok(()) => QueueEventResult::Queued,
        Err(mpsc::error::TrySendError::Full(_)) => {
            control.signal(Some(seq));
            QueueEventResult::Full
        }
        Err(mpsc::error::TrySendError::Closed(_)) => QueueEventResult::Closed,
    }
}

struct SessionForwarder {
    receiver: tokio::task::JoinHandle<()>,
    sender: tokio::task::JoinHandle<()>,
}

impl SessionForwarder {
    async fn stop(self) {
        self.sender.abort();
        self.receiver.abort();
        let _ = self.sender.await;
        let _ = self.receiver.await;
    }
}

enum ForwarderWake {
    Control(Option<()>),
    Event(Option<Box<(u64, Event)>>),
}

/// Minimum spacing between flushes of coalesced `AgentUpdated` events of agents
/// the connection did not subscribe to (2 Hz).
const COALESCE_INTERVAL: Duration = Duration::from_millis(500);

/// When the coalesced `AgentUpdated` events go out (see [`crate::interest::Router`]).
#[derive(Clone, Copy)]
enum FlushPlan {
    /// Nothing held.
    Idle,
    /// Held updates go out at this instant.
    At(tokio::time::Instant),
    /// The timer fired with no filtered seq to carry them: the next one flushes. No
    /// timer runs meanwhile, so an idle session stays idle.
    NextHole,
}

async fn flush_deadline(plan: FlushPlan) {
    match plan {
        FlushPlan::At(at) => tokio::time::sleep_until(at).await,
        FlushPlan::Idle | FlushPlan::NextHole => std::future::pending().await,
    }
}

/// Queue the coalesced updates that fit. Returns false once the sender task is gone.
fn queue_flushed(
    event_tx: &mpsc::Sender<(u64, Event)>,
    control: &ResyncControl,
    router: &mut crate::interest::Router,
) -> bool {
    for (seq, event) in router.flush() {
        if matches!(
            queue_event(event_tx, control, seq, event),
            QueueEventResult::Closed
        ) {
            return false;
        }
    }
    true
}

fn spawn_session_forwarder(
    mut receiver: broadcast::Receiver<(u64, Event)>,
    snapshot_seq: watch::Receiver<Option<u64>>,
    barrier: watch::Receiver<bool>,
    wire: WireSender,
    dropped_events: Arc<AtomicUsize>,
    interest: crate::interest::SharedInterest,
) -> SessionForwarder {
    let (control, mut control_rx) = ResyncControl::new();
    let sender_control = control.clone();
    let (event_tx, mut event_rx) = mpsc::channel(SESSION_FORWARD_EVENT_CAP);
    let receiver_task = tokio::spawn(async move {
        let mut deferred_event: Option<(u64, Event)> = None;
        // `Subscribe` filtering and 2 Hz coalescing of other agents' `AgentUpdated`;
        // the flush timer exists only while an update is held.
        let mut router = crate::interest::Router::new(interest);
        let mut flush = FlushPlan::Idle;
        'events: loop {
            let next = match deferred_event.take() {
                Some(event) => Ok(event),
                None => tokio::select! {
                    () = flush_deadline(flush) => {
                        if !queue_flushed(&event_tx, &control, &mut router) {
                            break 'events;
                        }
                        flush = if router.has_held() {
                            FlushPlan::NextHole
                        } else {
                            FlushPlan::Idle
                        };
                        continue 'events;
                    }
                    received = receiver.recv() => received,
                },
            };
            match next {
                Ok((seq, event)) => {
                    let Some((seq, event)) = router.route(seq, event) else {
                        if matches!(flush, FlushPlan::NextHole) {
                            if !queue_flushed(&event_tx, &control, &mut router) {
                                break;
                            }
                            flush = FlushPlan::Idle;
                        }
                        if router.has_held() && matches!(flush, FlushPlan::Idle) {
                            flush = FlushPlan::At(tokio::time::Instant::now() + COALESCE_INTERVAL);
                        }
                        continue;
                    };
                    if matches!(
                        queue_event(&event_tx, &control, seq, event),
                        QueueEventResult::Closed
                    ) {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    log::warn!("session event receiver lagged by {count} events");
                    router.reset();
                    flush = FlushPlan::Idle;
                    loop {
                        match receiver.try_recv() {
                            Ok((seq, event)) => {
                                control.signal(Some(seq));
                                deferred_event = Some((seq, event));
                                break;
                            }
                            Err(broadcast::error::TryRecvError::Lagged(count)) => {
                                log::warn!(
                                    "session event receiver remained behind by {count} events"
                                );
                            }
                            Err(broadcast::error::TryRecvError::Empty) => {
                                control.signal(None);
                                break;
                            }
                            Err(broadcast::error::TryRecvError::Closed) => break 'events,
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let sender_task = tokio::spawn(async move {
        let mut snapshot_seq = snapshot_seq;
        let snapshot_seq = loop {
            if let Some(snapshot_seq) = *snapshot_seq.borrow() {
                break snapshot_seq;
            }
            if snapshot_seq.changed().await.is_err() {
                return;
            }
        };
        let mut barrier = barrier;
        if !*barrier.borrow() && barrier.changed().await.is_err() {
            return;
        }
        while control_rx.try_recv().is_ok() {
            let Some(current_seq) = sender_control.take() else {
                continue;
            };
            wire.request_resync(current_seq);
        }
        let mut control_open = true;
        let mut event_open = true;
        let mut control_streak = 0;
        while control_open || event_open {
            let wake = if control_streak < MAX_CONTROL_BURST {
                tokio::select! {
                    biased;
                    control = control_rx.recv(), if control_open => ForwarderWake::Control(control),
                    next = event_rx.recv(), if event_open => ForwarderWake::Event(next.map(Box::new)),
                }
            } else {
                tokio::select! {
                    biased;
                    next = event_rx.recv(), if event_open => ForwarderWake::Event(next.map(Box::new)),
                    control = control_rx.recv(), if control_open => ForwarderWake::Control(control),
                }
            };
            match wake {
                ForwarderWake::Control(Some(())) => {
                    let Some(current_seq) = sender_control.take() else {
                        continue;
                    };
                    wire.request_resync(current_seq);
                    control_streak += 1;
                }
                ForwarderWake::Control(None) => control_open = false,
                ForwarderWake::Event(Some(next)) => {
                    let (seq, event) = *next;
                    if seq <= snapshot_seq {
                        continue;
                    }
                    if !wire.send_event(seq, event, &dropped_events).await {
                        break;
                    }
                    control_streak = 0;
                }
                ForwarderWake::Event(None) => event_open = false,
            }
        }
    });
    SessionForwarder {
        receiver: receiver_task,
        sender: sender_task,
    }
}

fn idempotent_session_id(client_id: &ClientId, idempotency_key: &str) -> SessionId {
    SessionId::new(format!(
        "pacode-attach-{}-{}-{}-{idempotency_key}",
        client_id.as_str().len(),
        client_id.as_str(),
        idempotency_key.len()
    ))
}

fn attach_fingerprint(attach: &Attach) -> Result<String, String> {
    serde_json::to_string(attach).map_err(|error| error.to_string())
}

async fn open_recorded_attach(
    core: &Core,
    record: &AttachAdmission,
    attach: &Attach,
    idempotency_key: &str,
) -> Result<SessionId, String> {
    match core
        .open_session(Attach::Resume {
            session: record.session_id.clone(),
        })
        .await
    {
        Ok(session) if session == record.session_id => Ok(session),
        Ok(session) => Err(format!(
            "idempotency key {idempotency_key:?} resolved session {session} instead of recorded {}",
            record.session_id
        )),
        Err(CoreError::SessionNotFound(_)) if record.allow_create => {
            let new_attach = match attach {
                Attach::New {
                    cwd,
                    model,
                    effort,
                    mode,
                } => Attach::New {
                    cwd: cwd.clone(),
                    model: model.clone(),
                    effort: *effort,
                    mode: *mode,
                },
                Attach::Latest { cwd } => Attach::New {
                    cwd: cwd.clone(),
                    model: None,
                    effort: None,
                    mode: None,
                },
                Attach::Resume { .. } => {
                    return Err("resume attach cannot recreate a missing session".to_string());
                }
            };
            let session = core
                .open_session_with_id(new_attach, record.session_id.clone())
                .await
                .map_err(|error| error.to_string())?;
            if session == record.session_id {
                return Ok(session);
            }
            Err(format!(
                "idempotency key {idempotency_key:?} resolved session {session} instead of recorded {}",
                record.session_id
            ))
        }
        Err(CoreError::SessionNotFound(_)) => Err(format!(
            "idempotency key {idempotency_key:?} points to missing session {}",
            record.session_id
        )),
        Err(error) => Err(error.to_string()),
    }
}

async fn open_idempotent_attach(
    core: &Core,
    store: &Store,
    client_id: &ClientId,
    idempotency_key: &str,
    attach: Attach,
) -> Result<SessionId, String> {
    let fingerprint = attach_fingerprint(&attach)?;
    if let Some(record) = store
        .get_attach_admission(client_id, idempotency_key)
        .await
        .map_err(|error| format!("attach admission lookup failed: {error}"))?
    {
        if record.fingerprint != fingerprint {
            return Err(format!(
                "idempotency key {idempotency_key:?} was already used with a different attach payload"
            ));
        }
        if record.allow_create
            && record.session_id != idempotent_session_id(client_id, idempotency_key)
        {
            return Err(format!(
                "idempotency key {idempotency_key:?} has an invalid recovery target {}",
                record.session_id
            ));
        }
        return open_recorded_attach(core, &record, &attach, idempotency_key).await;
    }

    let (session_id, allow_create) = match &attach {
        Attach::New { .. } => (idempotent_session_id(client_id, idempotency_key), true),
        Attach::Latest { cwd } => {
            let selected = store
                .list_sessions(SessionFilter {
                    cwd: Some(cwd.clone()),
                    limit: 1,
                })
                .await
                .map_err(|error| format!("latest session lookup failed: {error}"))?
                .into_iter()
                .next()
                .map(|session| session.id)
                .or_else(|| {
                    core.session_ids()
                        .into_iter()
                        .filter_map(|id| core.session(&id))
                        .filter(|session| session.meta().cwd == cwd.as_path())
                        .max_by_key(|session| session.meta().updated_at_ms)
                        .map(|session| session.id.clone())
                });
            selected.map_or(
                (idempotent_session_id(client_id, idempotency_key), true),
                |session| (session, false),
            )
        }
        Attach::Resume { .. } => {
            return Err(
                "idempotency keys are only supported for new and latest attach".to_string(),
            );
        }
    };

    let record = match store
        .admit_attach(
            client_id,
            idempotency_key,
            &fingerprint,
            &session_id,
            allow_create,
        )
        .await
        .map_err(|error| format!("attach admission failed: {error}"))?
    {
        AttachAdmissionResult::Admitted(record) | AttachAdmissionResult::Duplicate(record) => {
            record
        }
        AttachAdmissionResult::Conflict(_) => {
            return Err(format!(
                "idempotency key {idempotency_key:?} was already used with a different attach payload"
            ));
        }
    };
    if record.fingerprint != fingerprint {
        return Err(format!(
            "idempotency key {idempotency_key:?} was already used with a different attach payload"
        ));
    }
    if record.allow_create && record.session_id != idempotent_session_id(client_id, idempotency_key)
    {
        return Err(format!(
            "idempotency key {idempotency_key:?} has an invalid recovery target {}",
            record.session_id
        ));
    }
    open_recorded_attach(core, &record, &attach, idempotency_key).await
}

fn advance_attachment(generation: &Arc<AtomicU64>, cancellation: &mut CancellationToken) {
    cancellation.cancel();
    generation.fetch_add(1, Ordering::SeqCst);
    *cancellation = CancellationToken::new();
}

fn release_attach_lease(core: &Core, lease: &mut Option<SessionLease>) {
    if let Some(lease) = lease.take() {
        core.release_lease(&lease);
    }
}

async fn open_idempotent_attached(
    core: &Core,
    store: &Store,
    client_id: &ClientId,
    idempotency_key: &str,
    attach: Attach,
) -> Result<(SessionId, SessionLease), String> {
    for _ in 0..ATTACH_LEASE_ATTEMPTS {
        let session =
            open_idempotent_attach(core, store, client_id, idempotency_key, attach.clone()).await?;
        if let Ok(lease) = core.attach_lease(&session).await {
            return Ok((session, lease));
        }
    }
    Err("session could not be leased for attach".to_string())
}

async fn send_request_result(
    wire: &WireSender,
    result: Option<Result<RequestResult, tokio::task::JoinError>>,
    current_generation: u64,
) -> bool {
    match result {
        Some(Ok(result)) => {
            let reply = match result.reply {
                Some(reply) if !result.session_bound || result.generation == current_generation => {
                    reply
                }
                _ => Reply::Error {
                    message: "request cancelled by connection lifecycle transition".into(),
                },
            };
            wire.send_reply(result.id, reply).await
        }
        Some(Err(error)) => {
            log::warn!("request task failed: {error}");
            true
        }
        None => true,
    }
}

/// Best-effort `id` of an envelope that failed to parse (0 when absent).
fn request_id(line: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}

/// Serve one connection until EOF or shutdown.
pub async fn serve_connection(stream: UnixStream, core: Arc<Core>, control: ServerControl) {
    control
        .connections
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _guard = ConnectionGuard {
        connections: Arc::clone(&control.connections),
        disconnected: Arc::clone(&control.disconnected),
    };

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut input = Vec::new();
    let lifecycle = CancellationToken::new();
    let writer_finished = CancellationToken::new();
    let budget = Arc::new(Semaphore::new(WIRE_BYTE_BUDGET));
    let (data_tx, data_rx) = mpsc::channel(WIRE_QUEUE_CAP);
    let (resync, resync_rx) = ResyncControl::new();
    let writer_resync = resync.clone();
    let wire = WireSender {
        tx: data_tx,
        resync,
        budget,
        shutdown: control.shutdown.clone(),
        lifecycle: lifecycle.clone(),
    };
    let writer_handle = spawn_wire_writer(
        writer,
        data_rx,
        writer_resync,
        resync_rx,
        lifecycle,
        writer_finished.clone(),
    );
    let dropped_events = Arc::new(AtomicUsize::new(0));

    match read_handshake_line(&mut reader, &mut input, &control.shutdown, &writer_finished).await {
        HandshakeRead::Line => {}
        HandshakeRead::Closed | HandshakeRead::Cancelled => {
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
        HandshakeRead::WriterStopped => {
            log::warn!("client disconnected before handshake: wire writer stopped");
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
        HandshakeRead::Overflow => {
            log::warn!("protocol error: handshake line exceeds size limit");
            let _ = wire
                .send_reply(
                    0,
                    Reply::Error {
                        message: line_overflow_message(),
                    },
                )
                .await;
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
        HandshakeRead::TimedOut => {
            log::warn!("protocol error: handshake timed out");
            let _ = wire
                .send_reply(
                    0,
                    Reply::Error {
                        message: format!(
                            "handshake timed out after {}ms",
                            HANDSHAKE_TIMEOUT.as_millis()
                        ),
                    },
                )
                .await;
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
    }
    let envelope = match serde_json::from_slice::<AttachEnvelope>(&input) {
        Ok(envelope) => envelope,
        Err(error) => {
            log::warn!("protocol error: invalid first line JSON envelope: {error}");
            let _ = wire
                .send_reply(
                    0,
                    Reply::Error {
                        message: format!("invalid JSON envelope: {error}"),
                    },
                )
                .await;
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
    };
    let env = envelope.envelope;
    let hello = match env.req {
        Request::Hello(hello) => hello,
        _ => {
            log::warn!("protocol error: first request must be Hello");
            let _ = wire
                .send_reply(
                    env.id,
                    Reply::Error {
                        message: "first request must be Hello".to_string(),
                    },
                )
                .await;
            stop_wire_writer(wire, writer_handle).await;
            return;
        }
    };
    if hello.protocol != PROTOCOL_VERSION {
        log::warn!(
            "protocol version mismatch: client protocol is {}, daemon requires {PROTOCOL_VERSION}",
            hello.protocol
        );
        let _ = wire
            .send_reply(
                env.id,
                Reply::Error {
                    message: format!(
                        "protocol version mismatch: client protocol is {}, daemon requires {PROTOCOL_VERSION}",
                        hello.protocol
                    ),
                },
            )
            .await;
        stop_wire_writer(wire, writer_handle).await;
        return;
    }
    if !wire
        .send_reply(
            env.id,
            Reply::Hello {
                daemon_version: control.app_version.clone(),
                protocol: PROTOCOL_VERSION,
                pid: control.pid,
            },
        )
        .await
    {
        stop_wire_writer(wire, writer_handle).await;
        return;
    }

    log::info!(
        "client connected: client_id={} app_version={}",
        hello.client_id,
        hello.app_version
    );

    let mut attached_session: Option<SessionId> = None;
    // Which agents' high-volume events this connection wants; everything until `Subscribe`.
    let interest = crate::interest::new_shared();
    let mut attach_lease: Option<SessionLease> = None;
    let mut forwarder_handle: Option<SessionForwarder> = None;
    let global_handle = {
        let mut global_rx = core.subscribe_global();
        let event_wire = wire.clone();
        let dropped_events = dropped_events.clone();
        let global_core = Arc::clone(&core);
        tokio::spawn(async move {
            let mut last_seq = 0;
            loop {
                match global_rx.recv().await {
                    Ok((seq, event)) => {
                        last_seq = seq;
                        if !event_wire.send_event(seq, event, &dropped_events).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        log::warn!("global event receiver lagged by {count} events");
                        if !recover_global(
                            &mut global_rx,
                            &event_wire,
                            &dropped_events,
                            &mut last_seq,
                            || authoritative_global_events(&global_core),
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    let attachment_generation = Arc::new(AtomicU64::new(0));
    let mut attachment_cancel = CancellationToken::new();
    let mut background_tasks = JoinSet::new();
    let mut request_tasks = JoinSet::new();
    let mut pending_requests: VecDeque<RequestJob> = VecDeque::with_capacity(MAX_PENDING_REQUESTS);
    let mut control_streak = 0;

    'serve: loop {
        while let Some(result) = background_tasks.try_join_next() {
            if let Err(error) = result {
                log::warn!("background task failed: {error}");
            }
        }
        let current_generation = attachment_generation.load(Ordering::Acquire);
        while control_streak < MAX_CONTROL_BURST {
            let Some(result) = request_tasks.try_join_next() else {
                break;
            };
            control_streak += 1;
            if !send_request_result(&wire, Some(result), current_generation).await {
                break 'serve;
            }
        }
        while request_tasks.len() < MAX_IN_FLIGHT_REQUESTS {
            let Some(job) = pending_requests.pop_front() else {
                break;
            };
            if job.session.is_some() && job.generation != current_generation {
                if !wire
                    .send_reply(
                        job.id,
                        Reply::Error {
                            message: "request cancelled by connection lifecycle transition".into(),
                        },
                    )
                    .await
                {
                    break 'serve;
                }
                continue;
            }
            spawn_request(
                &core,
                job,
                &mut request_tasks,
                &attachment_generation,
                &attachment_cancel,
                &control.shutdown,
            );
        }
        tokio::select! {
            biased;
            _ = control.shutdown.cancelled() => break,
            _ = writer_finished.cancelled() => {
                log::warn!("client {} disconnected: wire writer stopped", hello.client_id);
                break;
            }
            read = read_input_line(&mut reader, &mut input) => {
                control_streak = 0;
                let line = match read {
                    Ok(()) => input.as_slice(),
                    Err(InputLineError::Overflow) => {
                        log::warn!("protocol error: input line exceeds size limit");
                        let _ = wire.send_reply(
                            0,
                            Reply::Error {
                                message: line_overflow_message(),
                            },
                        ).await;
                        break;
                    }
                    Err(InputLineError::Closed | InputLineError::Io) => break,
                };
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let envelope = match serde_json::from_slice::<AttachEnvelope>(line) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        // A request type this daemon does not know (newer client): answer
                        // it by id so the caller does not wait forever; the connection
                        // stays usable.
                        let id = std::str::from_utf8(line).map(request_id).unwrap_or(0);
                        log::warn!("protocol error: invalid JSON envelope: {error}");
                        if !wire.send_reply(
                            id,
                            Reply::Error {
                                message: format!("invalid JSON envelope: {error}"),
                            },
                        ).await {
                            break;
                        }
                        continue;
                    }
                };
                let idempotency_key = envelope.idempotency_key;
                let id = envelope.envelope.id;
                let request = match into_connection_request(envelope.envelope.req) {
                    Ok(request) => request,
                    Err(request) => {
                        let job = RequestJob {
                            id,
                            request,
                            session: attached_session.clone(),
                            generation: attachment_generation.load(Ordering::Acquire),
                        };
                        if request_tasks.len() < MAX_IN_FLIGHT_REQUESTS {
                            spawn_request(
                                &core,
                                job,
                                &mut request_tasks,
                                &attachment_generation,
                                &attachment_cancel,
                                &control.shutdown,
                            );
                        } else if pending_requests.len() < MAX_PENDING_REQUESTS {
                            pending_requests.push_back(job);
                        } else if !wire.send_reply(
                            id,
                            Reply::Error {
                                message: "request queue full".into(),
                            },
                        ).await {
                            break;
                        }
                        continue;
                    }
                };

                let mut attach_barrier = None;
                let mut cancel_shutdown_after_reply = false;
                let mut final_reply = false;
                let reply = match request {
                    ConnectionRequest::Hello => Reply::Error {
                        message: "already initialized".to_string(),
                    },
                    ConnectionRequest::Attach(attach) => {
                        advance_attachment(&attachment_generation, &mut attachment_cancel);
                        release_attach_lease(&core, &mut attach_lease);
                        log::info!("client {} attaching: attach={attach:?}", hello.client_id);
                        spawn_mcp_status_refresh(&core, &mut background_tasks);
                        if let Some(forwarder) = forwarder_handle.take() {
                            forwarder.stop().await;
                        }
                        attached_session = None;
                        {
                            let core = Arc::clone(&core);
                            let paths = control.paths.clone();
                            let reload = tokio::task::spawn_blocking(move || {
                                match pacode_config::load(&paths) {
                                    Ok(cfg) => {
                                        let mut api_keys = std::collections::BTreeMap::new();
                                        for (id, pcfg) in &cfg.providers {
                                            api_keys.insert(
                                                id.clone(),
                                                pacode_config::resolve_api_key(pcfg),
                                            );
                                        }
                                        if let Err(error) = core.reload_config(cfg, &api_keys) {
                                            log::warn!("config reload failed: {error}");
                                        }
                                    }
                                    Err(error) => log::warn!("config reload failed: {error}"),
                                }
                            });
                            tokio::pin!(reload);
                            tokio::select! {
                                biased;
                                _ = control.shutdown.cancelled() => break,
                                result = &mut reload => {
                                    if let Err(error) = result {
                                        log::warn!("config reload task failed: {error}");
                                    }
                                }
                            }
                        }
                        let open_session = async {
                            if let Some(idempotency_key) = idempotency_key
                                .as_deref()
                                .filter(|key| !key.is_empty())
                                && matches!(&attach, Attach::New { .. } | Attach::Latest { .. })
                            {
                                open_idempotent_attached(
                                    &core,
                                    &control.attach_store,
                                    &hello.client_id,
                                    idempotency_key,
                                    attach,
                                )
                                .await
                            } else {
                                core.open_attached(attach)
                                    .await
                                    .map_err(|error| error.to_string())
                            }
                        };
                        tokio::pin!(open_session);
                        let session = tokio::select! {
                            biased;
                            _ = control.shutdown.cancelled() => break,
                            result = &mut open_session => result,
                        };
                        match session {
                            Err(error) => {
                                log::warn!("client {} attach failed: {error}", hello.client_id);
                                Reply::Error { message: error }
                            }
                            Ok((session_id, lease)) => {
                                attach_lease = Some(lease);
                                log::info!(
                                    "client {} attached to session {session_id}",
                                    hello.client_id
                                );
                                match core.subscribe(&session_id) {
                                    None => {
                                        release_attach_lease(&core, &mut attach_lease);
                                        Reply::Error {
                                            message: "failed to subscribe to attached session"
                                                .to_string(),
                                        }
                                    }
                                    Some(events_rx) => {
                                        let (snapshot_tx, snapshot_rx) = watch::channel(None);
                                        let (barrier_tx, barrier_rx) = watch::channel(false);
                                        forwarder_handle = Some(spawn_session_forwarder(
                                            events_rx,
                                            snapshot_rx,
                                            barrier_rx,
                                            wire.clone(),
                                            dropped_events.clone(),
                                            Arc::clone(&interest),
                                        ));
                                        tokio::task::yield_now().await;
                                        match core.snapshot(&session_id) {
                                            None => {
                                                if let Some(forwarder) = forwarder_handle.take() {
                                                    forwarder.stop().await;
                                                }
                                                release_attach_lease(&core, &mut attach_lease);
                                                Reply::Error {
                                                    message:
                                                        "failed to retrieve snapshot for attached session"
                                                            .to_string(),
                                                }
                                            }
                                            Some(snapshot) => {
                                                if snapshot_tx.send(Some(snapshot.seq)).is_err() {
                                                    if let Some(forwarder) = forwarder_handle.take() {
                                                        forwarder.stop().await;
                                                    }
                                                    release_attach_lease(&core, &mut attach_lease);
                                                    Reply::Error {
                                                        message: "session event forwarder stopped"
                                                            .to_string(),
                                                    }
                                                } else {
                                                    attached_session = Some(session_id);
                                                    attach_barrier = Some(barrier_tx);
                                                    Reply::Attached(snapshot)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ConnectionRequest::Detach => {
                        advance_attachment(&attachment_generation, &mut attachment_cancel);
                        release_attach_lease(&core, &mut attach_lease);
                        log::info!(
                            "client {} detaching from session {attached_session:?}",
                            hello.client_id
                        );
                        spawn_mcp_status_refresh(&core, &mut background_tasks);
                        if let Some(forwarder) = forwarder_handle.take() {
                            forwarder.stop().await;
                        }
                        attached_session = None;
                        Reply::Ok
                    }
                    ConnectionRequest::CloseSession => {
                        advance_attachment(&attachment_generation, &mut attachment_cancel);
                        release_attach_lease(&core, &mut attach_lease);
                        if let Some(session_id) = attached_session.clone() {
                            log::info!("client {} closing session {session_id}", hello.client_id);
                            tokio::select! {
                                biased;
                                _ = control.shutdown.cancelled() => break,
                                _ = core.close_session(&session_id) => {}
                            }
                        }
                        if let Some(forwarder) = forwarder_handle.take() {
                            forwarder.stop().await;
                        }
                        attached_session = None;
                        Reply::Ok
                    }
                    ConnectionRequest::Ping => {
                        spawn_mcp_status_refresh(&core, &mut background_tasks);
                        Reply::Pong
                    }
                    ConnectionRequest::Subscribe { agents, all } => {
                        crate::interest::apply(&interest, agents, all);
                        Reply::Ok
                    }
                    ConnectionRequest::DebugStats => Reply::DebugStats(core.debug_stats()),
                    ConnectionRequest::Shutdown { force } => {
                        advance_attachment(&attachment_generation, &mut attachment_cancel);
                        cancel_shutdown_after_reply = true;
                        final_reply = true;
                        if !force {
                            control.graceful_shutdown.cancel();
                        }
                        Reply::Ok
                    }
                };

                let reply_sent = if final_reply {
                    wire.send_reply_final(id, reply).await
                } else {
                    wire.send_reply(id, reply).await
                };
                if let Some(barrier) = attach_barrier
                    && barrier.send(true).is_err()
                {
                    log::warn!("session event forwarder stopped before attach barrier release");
                }
                if cancel_shutdown_after_reply {
                    control.shutdown.cancel();
                    break;
                }
                if !reply_sent {
                    break;
                }
            }
            result = request_tasks.join_next(), if !request_tasks.is_empty() => {
                if !send_request_result(
                    &wire,
                    result,
                    attachment_generation.load(Ordering::Acquire),
                ).await {
                    break;
                }
            }
        }
    }

    attachment_cancel.cancel();
    release_attach_lease(&core, &mut attach_lease);
    background_tasks.abort_all();
    while background_tasks.join_next().await.is_some() {}
    request_tasks.abort_all();
    while request_tasks.join_next().await.is_some() {}

    if let Some(forwarder) = forwarder_handle.take() {
        forwarder.stop().await;
    }
    global_handle.abort();
    let _ = global_handle.await;
    log::info!("client {} disconnected", hello.client_id);
    stop_wire_writer(wire, writer_handle).await;
}

#[cfg(test)]
mod tests {
    use pacode_types::state::ToastLevel;

    use super::*;

    fn wire_channel(
        capacity: usize,
        budget: usize,
    ) -> (WireSender, mpsc::Receiver<OutboundLine>, mpsc::Receiver<()>) {
        let (tx, data_rx) = mpsc::channel(capacity);
        let (resync, resync_rx) = ResyncControl::new();
        let wire = WireSender {
            tx,
            resync,
            budget: Arc::new(Semaphore::new(budget)),
            shutdown: CancellationToken::new(),
            lifecycle: CancellationToken::new(),
        };
        (wire, data_rx, resync_rx)
    }

    fn test_wire(capacity: usize) -> (WireSender, mpsc::Receiver<String>, JoinHandle<()>) {
        let (wire, data_rx, resync_rx) = wire_channel(capacity, WIRE_BYTE_BUDGET);
        let writer_resync = wire.resync.clone();
        let lifecycle = wire.lifecycle.clone();
        let (wire_writer, wire_reader) = tokio::io::duplex(64 * 1024);
        let (line_tx, wire_rx) = mpsc::channel(capacity);
        let line_reader = tokio::spawn(async move {
            let mut lines = BufReader::new(wire_reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line_tx.send(line).await.is_err() {
                    break;
                }
            }
        });
        let writer = spawn_wire_writer(
            wire_writer,
            data_rx,
            writer_resync,
            resync_rx,
            lifecycle,
            CancellationToken::new(),
        );
        let tasks = tokio::spawn(async move {
            let _ = writer.await;
            line_reader.abort();
            let _ = line_reader.await;
        });
        (wire, wire_rx, tasks)
    }

    /// A shutdown reply is produced *after* the shutdown it answers was
    /// requested, so it must not be gated on the shutdown token. Deterministic
    /// version of the race the daemon integration test only hits under load.
    #[tokio::test]
    async fn the_final_line_is_sent_even_when_shutdown_is_already_cancelled() {
        let (wire, mut lines, tasks) = test_wire(4);
        wire.shutdown.cancel();

        assert!(
            wire.send_line_final("last line\n".to_string()).await,
            "the final line must be delivered, not refused by the shutdown token"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), lines.recv())
                .await
                .expect("final line never reached the wire"),
            Some("last line".to_string())
        );
        stop_wire_writer(wire, tasks).await;
    }

    /// The writer task must not tear itself down on the server-wide shutdown
    /// token: it owns the last line of the connection, and that line is
    /// enqueued after the shutdown was requested. Only `lifecycle` stops it.
    #[tokio::test]
    async fn the_writer_task_ignores_the_server_shutdown_token() {
        let (data_tx, data_rx) = mpsc::channel::<OutboundLine>(4);
        let (resync, resync_rx) = ResyncControl::new();
        let shutdown = CancellationToken::new();
        let lifecycle = CancellationToken::new();
        let budget = Arc::new(Semaphore::new(WIRE_BYTE_BUDGET));
        let (wire_writer, wire_reader) = tokio::io::duplex(64 * 1024);
        let writer = spawn_wire_writer(
            wire_writer,
            data_rx,
            resync,
            resync_rx,
            lifecycle.clone(),
            CancellationToken::new(),
        );

        async fn enqueue_line(
            budget: Arc<Semaphore>,
            data_tx: mpsc::Sender<OutboundLine>,
            text: &'static str,
        ) -> oneshot::Receiver<bool> {
            let (written_tx, written_rx) = oneshot::channel();
            let permit = budget
                .acquire_many_owned(wire_permits(text.len()))
                .await
                .unwrap();
            data_tx
                .send(OutboundLine {
                    text: text.to_string(),
                    written: Some(written_tx),
                    permit,
                })
                .await
                .unwrap();
            written_rx
        }

        // Cancel first, then enqueue: the writer is already parked in its
        // select with the shutdown token cancelled and must still take the line.
        shutdown.cancel();
        let mut reader = BufReader::new(wire_reader);
        let mut received = String::new();
        for text in ["first\n", "last\n"] {
            let written_rx = enqueue_line(Arc::clone(&budget), data_tx.clone(), text).await;
            assert!(
                tokio::time::timeout(Duration::from_secs(2), written_rx)
                    .await
                    .unwrap_or_else(|_| panic!("{text:?} was dropped after shutdown"))
                    .unwrap(),
                "{text:?} must be written after the shutdown token was cancelled"
            );
            reader.read_line(&mut received).await.unwrap();
            assert_eq!(received, text);
            received.clear();
        }

        // Only the connection-scoped token stops the writer.
        lifecycle.cancel();
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("the writer ignored its lifecycle token")
            .unwrap();
    }

    /// Counts `poll_write` calls; everything written lands in one buffer.
    #[derive(Clone, Default)]
    struct CountingWriter(Arc<Mutex<(Vec<u8>, usize)>>);

    impl AsyncWrite for CountingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let mut inner = self.0.lock().unwrap();
            inner.0.extend_from_slice(buf);
            inner.1 += 1;
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn queued_lines_share_one_write_and_each_is_acknowledged() {
        let (wire, data_rx, resync_rx) = wire_channel(16, WIRE_BYTE_BUDGET);
        let mut acks = Vec::new();
        let mut expected = String::new();
        for n in 0..5 {
            let text = format!("line-{n}\n");
            expected.push_str(&text);
            let (written_tx, written_rx) = oneshot::channel();
            let permit = wire.try_reserve_outbound(text.len()).unwrap();
            wire.tx
                .try_send(OutboundLine {
                    text,
                    written: Some(written_tx),
                    permit,
                })
                .unwrap_or_else(|_| panic!("queue full"));
            acks.push(written_rx);
        }
        let sink = CountingWriter::default();
        let writer = spawn_wire_writer(
            sink.clone(),
            data_rx,
            wire.resync.clone(),
            resync_rx,
            wire.lifecycle.clone(),
            CancellationToken::new(),
        );
        for ack in acks {
            let written = tokio::time::timeout(Duration::from_secs(1), ack)
                .await
                .expect("a batched line was never acknowledged")
                .unwrap();
            assert!(written);
        }
        {
            let inner = sink.0.lock().unwrap();
            assert_eq!(String::from_utf8(inner.0.clone()).unwrap(), expected);
            assert_eq!(inner.1, 1, "the queued lines were written one by one");
        }
        // Every permit went back to the budget after the flush.
        assert_eq!(wire.budget.available_permits(), WIRE_BYTE_BUDGET);
        stop_wire_writer(wire, writer).await;
    }

    #[tokio::test]
    async fn forwarder_filters_unsubscribed_agents_and_coalesces_their_updates() {
        let (event_tx, _) = broadcast::channel(64);
        let subscription = event_tx.subscribe();
        let (wire, mut wire_rx, writer) = test_wire(64);
        let (_snapshot_tx, snapshot_rx) = watch::channel(Some(0));
        let (_barrier_tx, barrier_rx) = watch::channel(true);
        let interest = crate::interest::new_shared();
        crate::interest::apply(&interest, vec![pacode_types::AgentId::main()], false);
        let forwarder = spawn_session_forwarder(
            subscription,
            snapshot_rx,
            barrier_rx,
            wire.clone(),
            Arc::new(AtomicUsize::new(0)),
            interest,
        );
        let sub = pacode_types::AgentId::new("sub-1");
        let agent = |activity: &str| pacode_types::AgentInfo {
            id: sub.clone(),
            name: "sub".into(),
            kind: pacode_types::AgentKind::Sub,
            status: pacode_types::AgentStatus::Thinking,
            activity: Some(activity.into()),
            started_at_ms: 0,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: pacode_types::ModelRoute::new("mock", "mock-model"),
            effort: pacode_types::Effort::Medium,
            parent: None,
            summary: None,
            error: None,
        };
        let delta = |agent: &pacode_types::AgentId| Event::TextDelta {
            agent: agent.clone(),
            item_seq: 1,
            text: "x".into(),
        };

        // The first update announces the agent; after that its stream is filtered and
        // its updates are held, while main's delta goes straight through.
        event_tx
            .send((1, Event::AgentUpdated(agent("start"))))
            .unwrap();
        for seq in 2..=20u64 {
            event_tx
                .send((seq, Event::AgentUpdated(agent(&format!("step {seq}")))))
                .unwrap();
        }
        event_tx
            .send((21, delta(&pacode_types::AgentId::main())))
            .unwrap();
        event_tx.send((22, delta(&sub))).unwrap();

        let mut received = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_500);
        while let Ok(Some(line)) = tokio::time::timeout_at(deadline, wire_rx.recv()).await {
            if let ServerMessage::Event { seq, event } = serde_json::from_str(line.trim()).unwrap()
            {
                received.push((seq, event));
            }
        }
        // The held update cannot go out as seq 20 after 21 was sent: it rides on the
        // filtered delta's seq 22, still carrying the latest state.
        let seqs: Vec<u64> = received.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, vec![1, 21, 22]);
        assert!(matches!(
            &received[2].1,
            Event::AgentUpdated(info) if info.activity.as_deref() == Some("step 20")
        ));
        forwarder.stop().await;
        stop_wire_writer(wire, writer).await;
    }

    #[test]
    fn attachment_lease_is_released_on_every_transition() {
        let generation = Arc::new(AtomicU64::new(0));
        let mut lease = CancellationToken::new();
        let alive = CancellationToken::new();

        assert!(attachment_lease_held(&generation, &lease, &alive, true, 0));

        advance_attachment(&generation, &mut lease);
        assert!(!attachment_lease_held(&generation, &lease, &alive, true, 0));
        assert!(attachment_lease_held(&generation, &lease, &alive, true, 1));
        assert!(attachment_lease_held(&generation, &lease, &alive, false, 0));

        let cancelled_shutdown = CancellationToken::new();
        cancelled_shutdown.cancel();
        assert!(!attachment_lease_held(
            &generation,
            &lease,
            &cancelled_shutdown,
            true,
            1
        ));

        lease.cancel();
        assert!(!attachment_lease_held(&generation, &lease, &alive, true, 1));
    }

    #[test]
    fn deterministic_attach_session_id_depends_on_both_unambiguously() {
        let first = idempotent_session_id(&ClientId::new("a"), "b-c");
        let second = idempotent_session_id(&ClientId::new("a-b"), "c");

        assert_ne!(first, second);
        assert_eq!(first, idempotent_session_id(&ClientId::new("a"), "b-c"));
    }

    #[tokio::test]
    async fn disconnect_notification_survives_wait_registration_gap() {
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let disconnected = Arc::new(tokio::sync::Notify::new());
        let waiting = disconnected.notified();

        {
            let _guard = ConnectionGuard {
                connections: Arc::clone(&connections),
                disconnected: Arc::clone(&disconnected),
            };
        }

        assert_eq!(connections.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn global_recovery_orders_retained_state_and_resync() {
        let session = SessionId::new("global-recovery");
        let (event_tx, mut receiver) = broadcast::channel(2);
        for (seq, status) in [
            (1, pacode_types::SessionStatusKind::Working),
            (2, pacode_types::SessionStatusKind::NeedsInput),
            (3, pacode_types::SessionStatusKind::Done),
        ] {
            event_tx
                .send((
                    seq,
                    Event::SessionStatus {
                        session: session.clone(),
                        status,
                    },
                ))
                .unwrap();
        }
        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(1))
        ));

        let (wire, mut wire_rx, writer) = test_wire(8);
        let mut last_seq = 0;
        assert!(
            recover_global(
                &mut receiver,
                &wire,
                &AtomicUsize::new(0),
                &mut last_seq,
                || {
                    vec![Event::SessionStatus {
                        session: session.clone(),
                        status: pacode_types::SessionStatusKind::Idle,
                    }]
                },
            )
            .await
        );

        for (expected_seq, expected_status) in [
            (2, pacode_types::SessionStatusKind::NeedsInput),
            (3, pacode_types::SessionStatusKind::Done),
            (4, pacode_types::SessionStatusKind::Idle),
        ] {
            let message: ServerMessage =
                tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
                    .await
                    .unwrap()
                    .map(|line| serde_json::from_str(line.trim()).unwrap())
                    .unwrap();
            assert!(matches!(
                message,
                ServerMessage::Event { seq, event: Event::SessionStatus { session: actual, status } }
                if seq == expected_seq && actual == session && status == expected_status
            ));
        }
        let marker: ServerMessage = tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
            .await
            .unwrap()
            .map(|line| serde_json::from_str(line.trim()).unwrap())
            .unwrap();
        assert!(matches!(
            marker,
            ServerMessage::Resync { current_seq: None }
        ));
        assert!(wire_rx.try_recv().is_err());

        event_tx
            .send((
                4,
                Event::SessionStatus {
                    session: session.clone(),
                    status: pacode_types::SessionStatusKind::Working,
                },
            ))
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await,
            Ok(Ok((4, Event::SessionStatus { .. })))
        ));
        stop_wire_writer(wire, writer).await;
    }

    #[test]
    fn resync_signal_coalesces_to_latest_sequence() {
        let (control, mut wake) = ResyncControl::new();
        control.signal(Some(4));
        control.signal(Some(9));

        assert_eq!(control.take(), Some(Some(9)));
        assert!(wake.try_recv().is_ok());
        assert!(wake.try_recv().is_err());
    }

    #[tokio::test]
    async fn attach_barrier_orders_reply_and_filters_snapshot_events() {
        let (event_tx, _) = broadcast::channel(16);
        let subscription = event_tx.subscribe();
        let (wire, mut wire_rx, writer) = test_wire(16);
        let (snapshot_tx, snapshot_rx) = watch::channel(None);
        let (barrier_tx, barrier_rx) = watch::channel(false);
        let forwarder = spawn_session_forwarder(
            subscription,
            snapshot_rx,
            barrier_rx,
            wire.clone(),
            Arc::new(AtomicUsize::new(0)),
            crate::interest::new_shared(),
        );

        event_tx
            .send((
                5,
                Event::Toast {
                    level: ToastLevel::Info,
                    title: "stale".into(),
                    detail: None,
                },
            ))
            .unwrap();
        event_tx
            .send((
                7,
                Event::Toast {
                    level: ToastLevel::Info,
                    title: "live".into(),
                    detail: None,
                },
            ))
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), wire_rx.recv())
                .await
                .is_err()
        );
        assert!(wire.send_reply(2, Reply::Ok).await);
        snapshot_tx.send(Some(6)).unwrap();
        barrier_tx.send(true).unwrap();

        let first: ServerMessage =
            serde_json::from_str(wire_rx.recv().await.unwrap().trim()).unwrap();
        assert!(matches!(
            first,
            ServerMessage::Reply {
                id: 2,
                reply: Reply::Ok
            }
        ));
        let second: ServerMessage =
            serde_json::from_str(wire_rx.recv().await.unwrap().trim()).unwrap();
        assert!(matches!(second, ServerMessage::Event { seq: 7, .. }));
        assert!(wire_rx.try_recv().is_err());
        forwarder.stop().await;
        stop_wire_writer(wire, writer).await;
    }

    #[tokio::test]
    async fn stale_inflight_result_is_fenced_by_generation() {
        let (wire, mut wire_rx, writer) = test_wire(4);
        let result = Some(Ok(RequestResult {
            id: 7,
            generation: 3,
            session_bound: true,
            reply: Some(Reply::Pong),
        }));

        assert!(send_request_result(&wire, result, 4).await);
        let message: ServerMessage =
            serde_json::from_str(wire_rx.recv().await.unwrap().trim()).unwrap();
        assert!(matches!(
            message,
            ServerMessage::Reply {
                id: 7,
                reply: Reply::Error { message }
            } if message.contains("cancelled")
        ));
        stop_wire_writer(wire, writer).await;
    }

    #[tokio::test]
    async fn resync_priority_does_not_starve_wire_data() {
        let (wire, mut wire_rx, writer) = test_wire(64);
        let producer_wire = wire.clone();
        let producer = tokio::spawn(async move {
            for seq in 1..=(MAX_CONTROL_BURST * 128) as u64 {
                producer_wire.request_resync(Some(seq));
                tokio::task::yield_now().await;
            }
        });
        let event_wire = wire.clone();
        let dropped_events = Arc::new(AtomicUsize::new(0));
        let send = tokio::spawn(async move {
            event_wire
                .send_event(
                    10_000,
                    Event::Toast {
                        level: ToastLevel::Info,
                        title: "data".into(),
                        detail: None,
                    },
                    &dropped_events,
                )
                .await
        });

        let mut saw_event = false;
        while !saw_event {
            let line = tokio::time::timeout(Duration::from_secs(2), wire_rx.recv())
                .await
                .expect("wire data was starved by resync control")
                .unwrap();
            let message: ServerMessage = serde_json::from_str(line.trim()).unwrap();
            saw_event = matches!(
                message,
                ServerMessage::Event {
                    seq: 10_000,
                    event: Event::Toast { .. }
                }
            );
        }
        producer.await.unwrap();
        assert!(send.await.unwrap());
        stop_wire_writer(wire, writer).await;
    }

    #[tokio::test]
    async fn bounded_input_rejects_oversized_line() {
        let payload = vec![b'x'; MAX_NDJSON_LINE + 1];
        let mut reader = BufReader::new(payload.as_slice());
        let mut line = Vec::new();

        assert!(matches!(
            read_input_line(&mut reader, &mut line).await,
            Err(InputLineError::Overflow)
        ));
    }

    #[tokio::test]
    async fn lossy_wire_overflow_requests_resync() {
        let (tx, _rx) = mpsc::channel(1);
        let (resync, _resync_rx) = ResyncControl::new();
        let wire = WireSender {
            tx,
            resync,
            budget: Arc::new(Semaphore::new(WIRE_BYTE_BUDGET)),
            shutdown: CancellationToken::new(),
            lifecycle: CancellationToken::new(),
        };
        let dropped_events = AtomicUsize::new(0);

        for seq in 1..=2 {
            assert!(
                wire.send_event(
                    seq,
                    Event::TextDelta {
                        agent: pacode_types::AgentId::main(),
                        item_seq: 1,
                        text: "delta".into(),
                    },
                    &dropped_events,
                )
                .await
            );
        }

        assert_eq!(dropped_events.load(Ordering::Relaxed), 1);
        assert_eq!(wire.resync.take(), Some(Some(2)));
    }

    #[tokio::test]
    async fn writer_failure_signals_connection_lifecycle() {
        let (wire, data_rx, resync_rx) = wire_channel(4, WIRE_BYTE_BUDGET);
        let finished = CancellationToken::new();
        let (writer_end, peer) = tokio::io::duplex(64);
        drop(peer);
        let writer = spawn_wire_writer(
            writer_end,
            data_rx,
            wire.resync.clone(),
            resync_rx,
            wire.lifecycle.clone(),
            finished.clone(),
        );

        assert!(!wire.send_reply(1, Reply::Ok).await);
        tokio::time::timeout(Duration::from_secs(1), finished.cancelled())
            .await
            .expect("writer termination was not propagated");
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .expect("writer task did not finish")
            .expect("writer task panicked");
    }

    #[tokio::test]
    async fn second_reply_waits_for_the_first_one_to_leave_the_budget() {
        let (wire, mut data_rx, _resync_rx) = wire_channel(8, 1);
        let first = tokio::spawn({
            let wire = wire.clone();
            async move { wire.send_reply(1, Reply::Ok).await }
        });
        let queued_first = tokio::time::timeout(Duration::from_secs(1), data_rx.recv())
            .await
            .expect("first reply was never queued")
            .expect("first reply was dropped");
        let second = tokio::spawn({
            let wire = wire.clone();
            async move { wire.send_reply(2, Reply::Pong).await }
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(50), data_rx.recv())
                .await
                .is_err(),
            "second reply was queued past the wire budget"
        );

        drop(queued_first);
        let queued_second = tokio::time::timeout(Duration::from_secs(1), data_rx.recv())
            .await
            .expect("second reply was never queued after the budget was released")
            .expect("second reply was dropped");
        assert!(queued_second.text.contains("\"id\":2"));

        assert!(!first.await.expect("first reply task panicked"));
        assert!(!second.is_finished());
        second.abort();
    }

    #[tokio::test]
    async fn lossy_delta_is_dropped_when_the_wire_budget_is_gone() {
        let first_delta = Event::TextDelta {
            agent: pacode_types::AgentId::main(),
            item_seq: 1,
            text: "x".repeat(WIRE_PERMIT_GRANULARITY),
        };
        let budget = wire_permits(
            serde_json::to_string(&ServerMessage::Event {
                seq: 1,
                event: first_delta.clone(),
            })
            .unwrap()
            .len()
                + 1,
        );
        let (wire, mut data_rx, _resync_rx) = wire_channel(8, usize::try_from(budget).unwrap());
        let dropped_events = AtomicUsize::new(0);

        assert!(wire.send_event(1, first_delta, &dropped_events).await);
        let queued = data_rx.try_recv().expect("first delta was not queued");
        assert!(
            wire.send_event(
                2,
                Event::TextDelta {
                    agent: pacode_types::AgentId::main(),
                    item_seq: 2,
                    text: "x".repeat(WIRE_PERMIT_GRANULARITY),
                },
                &dropped_events,
            )
            .await
        );

        assert_eq!(dropped_events.load(Ordering::Relaxed), 1);
        assert_eq!(wire.resync.take(), Some(Some(2)));
        drop(queued);
    }

    #[tokio::test]
    async fn oversized_reply_becomes_a_bounded_error() {
        let (wire, mut wire_rx, writer) = test_wire(4);

        assert!(
            wire.send_reply(
                9,
                Reply::Error {
                    message: "x".repeat(MAX_OUTBOUND_LINE + 1)
                }
            )
            .await
        );

        let line = tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
            .await
            .expect("bounded error reply was not written")
            .expect("bounded error reply was dropped");
        assert!(line.len() <= MAX_OUTBOUND_LINE);
        let message: ServerMessage = serde_json::from_str(line.trim()).unwrap();
        assert!(
            matches!(message, ServerMessage::Reply { id: 9, reply: Reply::Error { message } } if message.contains("wire limit"))
        );
        stop_wire_writer(wire, writer).await;
    }

    #[tokio::test]
    async fn oversized_events_are_dropped_and_request_resync() {
        let (wire, mut data_rx, _resync_rx) = wire_channel(4, WIRE_BYTE_BUDGET);
        let dropped_events = AtomicUsize::new(0);
        let payload = "x".repeat(MAX_OUTBOUND_LINE + 1);

        assert!(
            wire.send_event(
                3,
                Event::TextDelta {
                    agent: pacode_types::AgentId::main(),
                    item_seq: 1,
                    text: payload.clone(),
                },
                &dropped_events,
            )
            .await
        );
        assert!(
            wire.send_event(
                4,
                Event::Toast {
                    level: ToastLevel::Info,
                    title: "oversized".into(),
                    detail: Some(payload),
                },
                &dropped_events,
            )
            .await
        );

        assert_eq!(dropped_events.load(Ordering::Relaxed), 2);
        assert_eq!(wire.resync.take(), Some(Some(4)));
        assert!(data_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn lagged_receiver_sends_resync_before_retained_events() {
        let (event_tx, _) = broadcast::channel(4);
        let subscription = event_tx.subscribe();
        let (wire, mut wire_rx, writer) = test_wire(16);
        let (snapshot_tx, snapshot_rx) = watch::channel(Some(0));
        let (barrier_tx, barrier_rx) = watch::channel(true);
        snapshot_tx.send(Some(0)).unwrap();
        barrier_tx.send(true).unwrap();
        let forwarder = spawn_session_forwarder(
            subscription,
            snapshot_rx,
            barrier_rx,
            wire.clone(),
            Arc::new(AtomicUsize::new(0)),
            crate::interest::new_shared(),
        );

        for seq in 1..=32 {
            event_tx
                .send((
                    seq,
                    Event::Toast {
                        level: ToastLevel::Info,
                        title: format!("event-{seq}"),
                        detail: None,
                    },
                ))
                .unwrap();
        }

        let marker: ServerMessage = tokio::time::timeout(Duration::from_secs(1), async {
            let line = wire_rx.recv().await.unwrap();
            serde_json::from_str(line.trim()).unwrap()
        })
        .await
        .unwrap();
        assert!(matches!(
            marker,
            ServerMessage::Resync {
                current_seq: Some(29)
            }
        ));
        for seq in 29..=32 {
            let event: ServerMessage = tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
                .await
                .unwrap()
                .map(|line| serde_json::from_str(line.trim()).unwrap())
                .unwrap();
            assert!(matches!(event, ServerMessage::Event { seq: actual, .. } if actual == seq));
        }
        event_tx
            .send((
                33,
                Event::Toast {
                    level: ToastLevel::Info,
                    title: "event-33".into(),
                    detail: None,
                },
            ))
            .unwrap();
        let event: ServerMessage = tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
            .await
            .unwrap()
            .map(|line| serde_json::from_str(line.trim()).unwrap())
            .unwrap();
        assert!(matches!(event, ServerMessage::Event { seq: 33, .. }));
        forwarder.stop().await;
        stop_wire_writer(wire, writer).await;
    }
}
