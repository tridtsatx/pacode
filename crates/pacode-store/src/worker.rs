//! `Store`: async facade over a single SQLite writer thread.
//!
//! Implementation: `std::sync::mpsc` of boxed closures `FnOnce(&mut Connection) ->
//! Result<..>` with a `tokio::sync::oneshot` for the reply; the thread opens the
//! connection (WAL, `synchronous=NORMAL`, `cache_size=-2000`, `foreign_keys=ON`),
//! runs `schema::migrate`, then loops. `Store` is `Clone` (cheap); dropping the last
//! clone stops the thread.
//!
//! One writer per database file per process: `Store::open` hands out handles to a
//! process-wide worker registry keyed by the canonical file path, so a second open
//! of the same file joins the running writer instead of opening a second
//! connection to it.
//!
//! The loop is a group commit. Whatever has already queued up is taken as one
//! batch, runs inside one transaction with a savepoint per call, and is committed
//! once — and only after that commit is anyone told "done". The commit is the only
//! step that reaches the disk, so a hundred concurrent calls pay for it once
//! instead of a hundred times. That matters because the writer is a single thread
//! and everything in the daemon queues behind it: with 128 agents writing at once,
//! one fsync per call is the difference between a store that keeps up and a daemon
//! that is serialized behind the disk.
//!
//! `synchronous=NORMAL` is the other half of it, and the reason the batching is
//! worth having. Under `FULL` every commit fsyncs, so a batch of 64 still pays a
//! full fsync and grouping buys statement round-trips only. Under `NORMAL` in WAL
//! mode the commit is a write to the WAL and the fsync is paid at the next
//! checkpoint, across every write since. The trade is deliberate: a power cut can
//! lose the tail of the transcript, and a crash of the process cannot lose a byte
//! that was reported as done.

use std::any::Any;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, LazyLock, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc as std_mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

use pacode_types::{
    AgentId, AgentInfo, LatencyHist, Message, Plan, SessionId, SessionMeta, TaskId, TaskInfo,
    Usage, UsageTotals,
};
use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot, watch};

use crate::batch::{self, Answer, Batch, Command, Execution};
use crate::capabilities::{JournalMode, StorageCapabilities, StorageDialect, SynchronousMode};
use crate::queries::{
    AdmissionCursor, AttachAdmission, AttachAdmissionResult, MessageAdmission,
    MessageAdmissionPage, MessageAdmissionResult, MessagePage, MessageRow, SearchHit,
    SessionCursor, SessionFilter, SessionPage, TaskAdmission, TaskAdmissionResult,
};
use crate::{StoreError, queries, schema};

/// How many calls may be waiting for the writer before a send has to wait for
/// room. Two batches' worth, deliberately: a saturated queue then always has a
/// full batch waiting for the next commit, so no call waits behind a queue that
/// is being drained one commit at a time.
const QUEUE_CAPACITY: usize = 128;

/// How long a statement waits for a lock the writer does not hold — a reader on
/// another connection, or a second process with the same file open.
pub(crate) const BUSY_TIMEOUT_MS: u64 = 5_000;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 1_000;
const WAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

/// `PRAGMA synchronous` as SQLite reports it. `NORMAL` is the level the store
/// runs at, and the readback in [`Store::init_db`] is what makes sure the
/// connection is really there rather than merely asked to be.
const SQLITE_SYNCHRONOUS_NORMAL: i64 = 1;

/// Per-worker instrumentation. Everything here is an atomic updated on the
/// request path — no poller, no background thread — so an idle daemon pays
/// nothing and a spike is recorded even if it lasts one call.
///
/// `wait` and `exec` are the pair that answers "is the store slow, or is it
/// just backed up": `exec` is the cost of the work itself (for a write, mostly
/// the fsync that `synchronous=FULL` asks for), `wait` is how long a caller
/// queued behind other callers.
#[derive(Debug, Default)]
pub(crate) struct StoreMetrics {
    pub(crate) wait: LatencyHist,
    pub(crate) exec: LatencyHist,
    /// One sample per batch: the commit that carried it.
    pub(crate) commit: LatencyHist,
    pub(crate) enqueued: AtomicU64,
    pub(crate) completed: AtomicU64,
    /// Sends that found the queue closed or full.
    pub(crate) refused: AtomicU64,
    pub(crate) faults: AtomicU64,
    /// Commits run, whatever their depth. Divided into `completed` it is the
    /// mean batch size, which is the number that says whether grouping is
    /// actually happening.
    pub(crate) batches: AtomicU64,
    pub(crate) queue_depth: AtomicU64,
    pub(crate) queue_high_water: AtomicU64,
}

impl StoreMetrics {
    pub(crate) fn enqueued(&self) {
        self.enqueued.fetch_add(1, Ordering::Relaxed);
        let depth = self.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.queue_high_water.fetch_max(depth, Ordering::Relaxed);
    }

    pub(crate) fn dequeued(&self) {
        self.queue_depth.fetch_sub(1, Ordering::Relaxed);
        self.completed.fetch_add(1, Ordering::Relaxed);
    }
}
struct Inner {
    sender: mpsc::Sender<Command>,
    closing: AtomicBool,
    joined: watch::Receiver<bool>,
    joined_tx: watch::Sender<bool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    shared_key: Option<PathBuf>,
    fault: AtomicBool,
    metrics: Arc<StoreMetrics>,
}

fn shared_workers() -> &'static Mutex<HashMap<PathBuf, Weak<Inner>>> {
    static WORKERS: LazyLock<Mutex<HashMap<PathBuf, Weak<Inner>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &WORKERS
}

fn worker_key(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or(path.as_os_str());
    match std::fs::canonicalize(parent) {
        Ok(parent) => parent.join(name),
        Err(_) => path.to_path_buf(),
    }
}

fn reuse_worker(key: &Path) -> Option<Arc<Inner>> {
    let mut workers = shared_workers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let inner = workers.get(key).and_then(Weak::upgrade)?;
    if inner.closing.load(Ordering::Acquire) {
        return None;
    }
    workers.retain(|_, weak| weak.strong_count() > 0);
    Some(inner)
}

fn register_worker(key: &Path, inner: &Arc<Inner>) {
    shared_workers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.to_path_buf(), Arc::downgrade(inner));
}

fn unregister_worker(key: &Path, inner: &Arc<Inner>) {
    let mut workers = shared_workers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ours = workers
        .get(key)
        .and_then(Weak::upgrade)
        .is_some_and(|current| Arc::ptr_eq(&current, inner));
    if ours {
        workers.remove(key);
    }
}

fn journal_mode_from_str(value: &str) -> JournalMode {
    match value.trim() {
        "delete" => JournalMode::Delete,
        "truncate" => JournalMode::Truncate,
        "persist" => JournalMode::Persist,
        "memory" => JournalMode::Memory,
        "wal" => JournalMode::Wal,
        _ => JournalMode::Other,
    }
}

fn synchronous_from_i64(value: i64) -> SynchronousMode {
    match value {
        0 => SynchronousMode::Off,
        1 => SynchronousMode::Normal,
        2 => SynchronousMode::Full,
        3 => SynchronousMode::Extra,
        _ => SynchronousMode::Other,
    }
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl Inner {
    fn reap(&self) {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(handle) = handle else {
            return;
        };

        let joined_tx = self.joined_tx.clone();
        let reaper = std::thread::Builder::new()
            .name("pacode-store-reaper".to_string())
            .spawn(move || {
                let _ = handle.join();
                let _ = joined_tx.send(true);
            });
        if reaper.is_err() {
            let _ = self.joined_tx.send(true);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let handle = self.handle.get_mut().ok().and_then(|handle| handle.take());
        let Some(handle) = handle else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("pacode-store-reaper".to_string())
            .spawn(move || {
                let _ = handle.join();
            });
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let is_memory = path.to_str() == Some(":memory:");
        Self::start_worker(path.to_path_buf(), is_memory)
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Self::start_worker(PathBuf::from(":memory:"), true)
    }

    #[cfg(debug_assertions)]
    pub fn set_fault_injection(&self, enabled: bool) {
        self.inner.fault.store(enabled, Ordering::Release);
    }

    fn start_worker(path: PathBuf, is_memory: bool) -> Result<Store, StoreError> {
        let shared_key = (!is_memory).then(|| worker_key(&path));
        if let Some(key) = shared_key.as_deref()
            && let Some(inner) = reuse_worker(key)
        {
            return Ok(Store { inner });
        }

        let (init_tx, init_rx) = std_mpsc::sync_channel::<Result<(), StoreError>>(1);
        let (tx, rx) = mpsc::channel::<Command>(QUEUE_CAPACITY);
        let (joined_tx, joined_rx) = watch::channel(false);

        let inner = Arc::new(Inner {
            sender: tx,
            closing: AtomicBool::new(false),
            joined: joined_rx,
            joined_tx,
            handle: Mutex::new(None),
            shared_key: shared_key.clone(),
            fault: AtomicBool::new(false),
            metrics: Arc::new(StoreMetrics::default()),
        });

        let worker_inner = Arc::downgrade(&inner);
        // A handle on the instrumentation, not on `Inner`: holding `Inner` here
        // would keep the sender — and so this very thread — alive for as long as
        // the writer runs.
        let worker_metrics = Arc::clone(&inner.metrics);
        let builder = std::thread::Builder::new().name("pacode-store".to_string());
        let handle = builder
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    Self::run_worker(path, is_memory, worker_inner, init_tx, rx, worker_metrics);
                }));
            })
            .map_err(StoreError::Io)?;

        *inner
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
        if let Some(key) = shared_key.as_deref() {
            register_worker(key, &inner);
        }

        init_rx.recv().map_err(|_| StoreError::Closed)??;

        Ok(Store { inner })
    }

    fn run_worker(
        path: PathBuf,
        is_memory: bool,
        inner: Weak<Inner>,
        init_tx: std_mpsc::SyncSender<Result<(), StoreError>>,
        mut rx: mpsc::Receiver<Command>,
        metrics: Arc<StoreMetrics>,
    ) {
        let conn_res = if is_memory {
            Connection::open_in_memory()
        } else {
            Connection::open(&path)
        };

        let mut conn = match conn_res {
            Ok(conn) => conn,
            Err(error) => {
                let _ = init_tx.send(Err(StoreError::Sqlite(error)));
                return;
            }
        };

        if let Err(error) = Self::init_db(&mut conn, is_memory) {
            let _ = init_tx.send(Err(error));
            return;
        }

        if init_tx.send(Ok(())).is_err() {
            return;
        }

        while let Some(first) = rx.blocking_recv() {
            let Batch {
                executions,
                close_after,
            } = batch::collect_batch(&mut rx, first);
            batch::run_batch(&mut conn, executions, is_memory, &metrics);
            if close_after {
                rx.close();
                break;
            }
        }

        Self::finish_db(&mut conn, is_memory);

        if let Some(inner) = inner.upgrade()
            && let Some(key) = inner.shared_key.clone()
        {
            unregister_worker(&key, &inner);
        }
    }

    fn init_db(conn: &mut Connection, is_memory: bool) -> Result<(), StoreError> {
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        if !is_memory {
            let mode: String =
                conn.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0))?;
            if !mode.trim().eq_ignore_ascii_case("wal") {
                return Err(StoreError::Invalid(format!(
                    "sqlite refused WAL journaling: journal_mode = {mode}"
                )));
            }
        }
        conn.execute_batch(&format!(
            r#"
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -2000;
            PRAGMA foreign_keys = ON;
            PRAGMA wal_autocheckpoint = {WAL_AUTOCHECKPOINT_PAGES};
            PRAGMA journal_size_limit = {WAL_SIZE_LIMIT_BYTES};
            "#,
        ))?;
        // Read back rather than trust the pragma: a build of SQLite that refused
        // it would otherwise silently run the store at the durability level the
        // pragma asked for before this line, with nothing reporting the gap.
        let synchronous: i64 = conn.query_row("PRAGMA synchronous;", [], |row| row.get(0))?;
        if !is_memory && synchronous != SQLITE_SYNCHRONOUS_NORMAL {
            return Err(StoreError::Invalid(format!(
                "sqlite refused synchronous = NORMAL: synchronous = {synchronous}"
            )));
        }
        schema::migrate(conn)?;
        Ok(())
    }

    /// What the open connection actually offers, read back from the live pragmas.
    /// A file store is expected to satisfy [`DialectRequirements::PACODE`];
    /// [`Store::open_in_memory`] cannot offer WAL and reports the gap instead of
    /// pretending otherwise.
    ///
    /// [`DialectRequirements::PACODE`]: crate::capabilities::DialectRequirements::PACODE
    pub async fn storage_capabilities(&self) -> Result<StorageCapabilities, StoreError> {
        self.call(|conn| {
            let journal_mode: String =
                conn.query_row("PRAGMA journal_mode;", [], |row| row.get(0))?;
            let synchronous: i64 = conn.query_row("PRAGMA synchronous;", [], |row| row.get(0))?;
            let fts5 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'pacode_fts5_probe';",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .is_ok();
            Ok(StorageCapabilities {
                dialect: StorageDialect::Sqlite,
                fts5,
                journal_mode: journal_mode_from_str(&journal_mode),
                synchronous: synchronous_from_i64(synchronous),
                transactions: true,
                keyset_pagination: true,
            })
        })
        .await
    }

    /// The future this hands back owns a clone of the writer's handle and the
    /// call itself — the precise-capture list says so, which is what lets a
    /// caller queue a call, drop its `Store`, and still get an answer.
    fn call<T: Send + 'static, F>(
        &self,
        f: F,
    ) -> impl std::future::Future<Output = Result<T, StoreError>> + use<T, F>
    where
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let inner = self.inner.clone();
        async move {
            if inner.closing.load(Ordering::Acquire) {
                return Err(StoreError::Closed);
            }
            if inner.fault.load(Ordering::Acquire) {
                return Err(StoreError::Invalid("injected store failure".into()));
            }

            let (tx, rx) = oneshot::channel::<Answer>();
            let command = Command::Execute(Execution {
                // Stamped here, read inside the writer: the gap between the two
                // is exactly the time this caller spent queued behind others.
                enqueued_at: std::time::Instant::now(),
                task: Box::new(move |conn| Ok(Box::new(f(conn)?) as Box<dyn Any + Send>)),
                reply: tx,
            });

            inner.metrics.enqueued();
            if inner.sender.send(command).await.is_err() {
                inner.metrics.dequeued();
                inner.metrics.refused.fetch_add(1, Ordering::Relaxed);
                return Err(StoreError::Closed);
            }
            let value = rx.await.map_err(|_| StoreError::Closed)??;
            Ok(*value.downcast::<T>().map_err(|_| {
                StoreError::Invalid("store writer returned another call's answer".to_string())
            })?)
        }
    }

    /// A reading of this worker's instrumentation. Cheap enough to call from a
    /// request handler: a fixed number of atomic loads and no locking.
    pub fn stats(&self) -> pacode_types::StoreStats {
        let metrics = &self.inner.metrics;
        pacode_types::StoreStats {
            enqueued: metrics.enqueued.load(Ordering::Relaxed),
            completed: metrics.completed.load(Ordering::Relaxed),
            refused: metrics.refused.load(Ordering::Relaxed),
            faults: metrics.faults.load(Ordering::Relaxed),
            batches: metrics.batches.load(Ordering::Relaxed),
            queue_depth: metrics.queue_depth.load(Ordering::Relaxed),
            queue_high_water: metrics.queue_high_water.load(Ordering::Relaxed),
            wait: metrics.wait.snapshot(),
            exec: metrics.exec.snapshot(),
            commit: metrics.commit.snapshot(),
        }
    }

    fn finish_db(conn: &mut Connection, is_memory: bool) {
        if is_memory {
            return;
        }
        batch::rollback_if_needed(conn);
        if !batch::checkpoint_wal(conn, true) {
            batch::checkpoint_wal(conn, false);
        }
    }

    // --- sessions ---
    pub async fn upsert_session(&self, meta: &SessionMeta) -> Result<(), StoreError> {
        let meta = meta.clone();
        self.call(move |conn| queries::upsert_session(conn, &meta))
            .await
    }

    pub async fn get_session(&self, id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
        let id = id.clone();
        self.call(move |conn| queries::get_session(conn, &id)).await
    }

    /// Most recently updated first.
    pub async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionMeta>, StoreError> {
        self.call(move |conn| queries::list_sessions(conn, filter))
            .await
    }

    pub async fn list_sessions_page(
        &self,
        filter: &SessionFilter,
        cursor: Option<&SessionCursor>,
    ) -> Result<SessionPage, StoreError> {
        let filter = filter.clone();
        let cursor = cursor.cloned();
        self.call(move |conn| queries::list_sessions_page(conn, &filter, cursor.as_ref()))
            .await
    }

    pub async fn get_attach_admission(
        &self,
        client_id: &pacode_types::ClientId,
        idempotency_key: &str,
    ) -> Result<Option<AttachAdmission>, StoreError> {
        let client_id = client_id.clone();
        let idempotency_key = idempotency_key.to_string();
        self.call(move |conn| queries::get_attach_admission(conn, &client_id, &idempotency_key))
            .await
    }

    pub async fn admit_attach(
        &self,
        client_id: &pacode_types::ClientId,
        idempotency_key: &str,
        fingerprint: &str,
        session_id: &SessionId,
        allow_create: bool,
    ) -> Result<AttachAdmissionResult, StoreError> {
        let client_id = client_id.clone();
        let idempotency_key = idempotency_key.to_string();
        let fingerprint = fingerprint.to_string();
        let session_id = session_id.clone();
        self.call(move |conn| {
            queries::admit_attach(
                conn,
                &client_id,
                &idempotency_key,
                &fingerprint,
                &session_id,
                allow_create,
            )
        })
        .await
    }

    /// Deletes the session and everything owned by it.
    pub async fn delete_session(&self, id: &SessionId) -> Result<(), StoreError> {
        let id = id.clone();
        self.call(move |conn| queries::delete_session(conn, &id))
            .await
    }

    // --- messages ---
    /// `seq` is per (session, agent) and assigned by the core.
    pub async fn append_message(
        &self,
        session: &SessionId,
        agent: &AgentId,
        seq: u64,
        msg: &Message,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        let msg = msg.clone();
        self.call(move |conn| queries::append_message(conn, &session, &agent, seq, &msg))
            .await
    }

    pub async fn append_user_message(
        &self,
        session: &SessionId,
        agent: &AgentId,
        seq: u64,
        message_id: &str,
        fingerprint: u64,
        msg: &Message,
    ) -> Result<MessageAdmissionResult, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        let message_id = message_id.to_string();
        let msg = msg.clone();
        self.call(move |conn| {
            queries::append_user_message(
                conn,
                &session,
                &agent,
                seq,
                &message_id,
                fingerprint,
                &msg,
            )
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_user_message_with_turn(
        &self,
        session: &SessionId,
        agent: &AgentId,
        seq: u64,
        message_id: &str,
        fingerprint: u64,
        turn_id: Option<&str>,
        msg: &Message,
    ) -> Result<MessageAdmissionResult, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        let message_id = message_id.to_string();
        let turn_id = turn_id.map(str::to_string);
        let msg = msg.clone();
        self.call(move |conn| {
            queries::append_user_message_with_turn(
                conn,
                &session,
                &agent,
                seq,
                &message_id,
                fingerprint,
                turn_id.as_deref(),
                &msg,
            )
        })
        .await
    }

    pub async fn get_message_admission(
        &self,
        session: &SessionId,
        message_id: &str,
    ) -> Result<Option<MessageAdmission>, StoreError> {
        let session = session.clone();
        let message_id = message_id.to_string();
        self.call(move |conn| queries::get_message_admission(conn, &session, &message_id))
            .await
    }

    pub async fn load_message_admissions(&self) -> Result<Vec<MessageAdmission>, StoreError> {
        self.call(|conn| queries::load_message_admissions(conn))
            .await
    }

    pub async fn load_message_admissions_for_session(
        &self,
        session: &SessionId,
    ) -> Result<Vec<MessageAdmission>, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::load_message_admissions_for_session(conn, &session))
            .await
    }

    pub async fn load_unresolved_message_admissions(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Vec<MessageAdmission>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::load_unresolved_message_admissions(conn, &session, &agent))
            .await
    }

    pub async fn load_message_admissions_page(
        &self,
        limit: u32,
        cursor: Option<&AdmissionCursor>,
    ) -> Result<MessageAdmissionPage, StoreError> {
        let cursor = cursor.cloned();
        self.call(move |conn| queries::load_message_admissions_page(conn, limit, cursor.as_ref()))
            .await
    }

    pub async fn mark_message_admission_running(
        &self,
        session: &SessionId,
        message_id: &str,
        turn_id: &str,
    ) -> Result<bool, StoreError> {
        let session = session.clone();
        let message_id = message_id.to_string();
        let turn_id = turn_id.to_string();
        self.call(move |conn| {
            queries::mark_message_admission_running(conn, &session, &message_id, &turn_id)
        })
        .await
    }

    pub async fn mark_message_admission_terminal(
        &self,
        session: &SessionId,
        message_id: &str,
        terminal_stop: Option<&str>,
    ) -> Result<bool, StoreError> {
        let session = session.clone();
        let message_id = message_id.to_string();
        let terminal_stop = terminal_stop.map(str::to_string);
        self.call(move |conn| {
            queries::mark_message_admission_terminal(
                conn,
                &session,
                &message_id,
                terminal_stop.as_deref(),
            )
        })
        .await
    }

    pub async fn cancelled_message_seqs(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<std::collections::HashSet<u64>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::cancelled_message_seqs(conn, &session, &agent))
            .await
    }

    /// All messages of an agent in seq order.
    pub async fn load_messages(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::load_messages(conn, &session, &agent))
            .await
    }

    /// Up to `limit` messages with `seq < before` (or the last ones), ascending.
    pub async fn load_messages_before(
        &self,
        session: &SessionId,
        agent: &AgentId,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::load_messages_before(conn, &session, &agent, before, limit))
            .await
    }

    /// Up to `limit` live messages with `seq > after`, ascending: messages of
    /// admissions that ended interrupted, cancelled or skipped are skipped and the
    /// window is refilled past them, so a short page means the history ran out.
    pub async fn load_messages_after(
        &self,
        session: &SessionId,
        agent: &AgentId,
        after: Option<u64>,
        limit: u32,
    ) -> Result<MessagePage, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::load_messages_after(conn, &session, &agent, after, limit))
            .await
    }

    pub async fn max_message_seq(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Option<u64>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::max_message_seq(conn, &session, &agent))
            .await
    }

    // --- agents ---
    pub async fn upsert_agent(
        &self,
        session: &SessionId,
        info: &AgentInfo,
        prompt: Option<&str>,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let info = info.clone();
        let prompt = prompt.map(str::to_string);
        self.call(move |conn| queries::upsert_agent(conn, &session, &info, prompt.as_deref()))
            .await
    }

    pub async fn create_agent_with_history(
        &self,
        session: &SessionId,
        info: &AgentInfo,
        prompt: Option<&str>,
        rows: &[(u64, Arc<Message>)],
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let info = info.clone();
        let prompt = prompt.map(str::to_string);
        let rows = rows.to_vec();
        self.call(move |conn| {
            queries::create_agent_with_history(conn, &session, &info, prompt.as_deref(), &rows)
        })
        .await
    }

    pub async fn agent_prompt(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Option<String>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::agent_prompt(conn, &session, &agent))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn detach_agent(
        &self,
        session: &SessionId,
        info: &AgentInfo,
        prompt: Option<&str>,
        rows: &[(u64, Arc<Message>)],
        retained_seqs: &[u64],
        summary_upto: Option<u64>,
        old_owner: &AgentId,
        new_owner: &AgentId,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let info = info.clone();
        let prompt = prompt.map(str::to_string);
        let rows = rows.to_vec();
        let retained_seqs = retained_seqs.to_vec();
        let old_owner = old_owner.clone();
        let new_owner = new_owner.clone();
        self.call(move |conn| {
            queries::detach_agent(
                conn,
                &session,
                &info,
                prompt.as_deref(),
                &rows,
                &retained_seqs,
                summary_upto,
                &old_owner,
                &new_owner,
            )
        })
        .await
    }

    pub async fn rollback_detach(
        &self,
        session: &SessionId,
        detached: &AgentId,
        rows: &[(u64, Arc<Message>)],
        old_owner: &AgentId,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let detached = detached.clone();
        let rows = rows.to_vec();
        let old_owner = old_owner.clone();
        self.call(move |conn| {
            queries::rollback_detach(conn, &session, &detached, &rows, &old_owner)
        })
        .await
    }

    pub async fn list_agents(&self, session: &SessionId) -> Result<Vec<AgentInfo>, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::list_agents(conn, &session))
            .await
    }

    // --- tasks ---
    pub async fn upsert_task(&self, info: &TaskInfo) -> Result<(), StoreError> {
        let info = info.clone();
        self.call(move |conn| queries::upsert_task(conn, &info))
            .await
    }

    pub async fn upsert_task_checked(&self, info: &TaskInfo) -> Result<bool, StoreError> {
        let info = info.clone();
        self.call(move |conn| queries::upsert_task_checked(conn, &info))
            .await
    }

    pub async fn admit_task(
        &self,
        session: &SessionId,
        idempotency_key: &str,
        fingerprint: &str,
        task_id: &TaskId,
        owner: &AgentId,
    ) -> Result<TaskAdmissionResult, StoreError> {
        let session = session.clone();
        let idempotency_key = idempotency_key.to_string();
        let fingerprint = fingerprint.to_string();
        let task_id = task_id.clone();
        let owner = owner.clone();
        self.call(move |conn| {
            queries::admit_task(
                conn,
                &session,
                &idempotency_key,
                &fingerprint,
                &task_id,
                &owner,
            )
        })
        .await
    }

    pub async fn get_task_admission(
        &self,
        session: &SessionId,
        idempotency_key: &str,
    ) -> Result<Option<TaskAdmission>, StoreError> {
        let session = session.clone();
        let idempotency_key = idempotency_key.to_string();
        self.call(move |conn| queries::get_task_admission(conn, &session, &idempotency_key))
            .await
    }

    pub async fn get_task_admission_by_task(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<TaskAdmission>, StoreError> {
        let task_id = task_id.clone();
        self.call(move |conn| queries::get_task_admission_by_task(conn, &task_id))
            .await
    }

    pub async fn list_tasks(&self, session: &SessionId) -> Result<Vec<TaskInfo>, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::list_tasks(conn, &session))
            .await
    }

    pub async fn reassign_task_owner(
        &self,
        session: &SessionId,
        from: &AgentId,
        to: &AgentId,
    ) -> Result<Vec<TaskInfo>, StoreError> {
        let session = session.clone();
        let from = from.clone();
        let to = to.clone();
        self.call(move |conn| queries::reassign_task_owner(conn, &session, &from, &to))
            .await
    }

    // --- cron jobs ---
    pub async fn upsert_cron_job(
        &self,
        session: &SessionId,
        job: &pacode_types::CronJob,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let job = job.clone();
        self.call(move |conn| queries::upsert_cron_job(conn, &session, &job))
            .await
    }

    pub async fn delete_cron_job(&self, id: &pacode_types::CronJobId) -> Result<bool, StoreError> {
        let id = id.clone();
        self.call(move |conn| queries::delete_cron_job(conn, &id))
            .await
    }

    pub async fn list_cron_jobs(
        &self,
        session: &SessionId,
    ) -> Result<Vec<pacode_types::CronJob>, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::list_cron_jobs(conn, &session))
            .await
    }

    // --- plan ---
    pub async fn save_plan(&self, session: &SessionId, plan: &Plan) -> Result<(), StoreError> {
        let session = session.clone();
        let plan = plan.clone();
        self.call(move |conn| queries::save_plan(conn, &session, &plan))
            .await
    }

    pub async fn load_plan(&self, session: &SessionId) -> Result<Plan, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::load_plan(conn, &session))
            .await
    }

    // --- usage ---
    pub async fn add_usage(
        &self,
        session: &SessionId,
        agent: &AgentId,
        turn: u32,
        usage: &Usage,
        cost_usd: Option<f64>,
    ) -> Result<(), StoreError> {
        self.add_usage_for_turn(session, agent, turn, None, usage, cost_usd)
            .await
            .map(|_| ())
    }

    pub async fn add_usage_for_turn(
        &self,
        session: &SessionId,
        agent: &AgentId,
        turn: u32,
        turn_id: Option<&str>,
        usage: &Usage,
        cost_usd: Option<f64>,
    ) -> Result<bool, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        let turn_id = turn_id.map(str::to_string);
        let usage = *usage;
        self.call(move |conn| {
            queries::add_usage_for_turn(
                conn,
                &session,
                &agent,
                turn,
                turn_id.as_deref(),
                &usage,
                cost_usd,
            )
        })
        .await
    }

    /// Sums over the session; `context_*` fields stay zero (the core fills them).
    pub async fn usage_totals(&self, session: &SessionId) -> Result<UsageTotals, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::usage_totals(conn, &session))
            .await
    }

    // --- compaction ---
    pub async fn save_compaction(
        &self,
        session: &SessionId,
        agent: &AgentId,
        summary: &str,
        upto_seq: u64,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        let summary = summary.to_string();
        self.call(move |conn| queries::save_compaction(conn, &session, &agent, &summary, upto_seq))
            .await
    }

    pub async fn load_compaction(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::load_compaction(conn, &session, &agent))
            .await
    }

    // --- search ---
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, StoreError> {
        let query = query.to_string();
        self.call(move |conn| queries::search(conn, &query, limit))
            .await
    }

    /// Flush and stop the writer thread. A store shared with other handles keeps
    /// its writer alive; the last handle to close stops it.
    pub async fn close(self) {
        if Arc::strong_count(&self.inner) > 1 {
            return;
        }
        let first_close = !self.inner.closing.swap(true, Ordering::AcqRel);
        self.inner.reap();

        if first_close {
            let sender = self.inner.sender.clone();
            let close_thread = std::thread::Builder::new()
                .name("pacode-store-close".to_string())
                .spawn(move || {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        let _ = sender.blocking_send(Command::Close);
                    }));
                });
            if close_thread.is_err() {
                let _ = self.inner.sender.send(Command::Close).await;
            }
        }

        let mut joined = self.inner.joined.clone();
        loop {
            if *joined.borrow_and_update() {
                break;
            }
            if joined.changed().await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
