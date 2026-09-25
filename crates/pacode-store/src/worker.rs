//! `Store`: async facade over a single SQLite writer thread.
//!
//! Implementation: `std::sync::mpsc` of boxed closures `FnOnce(&mut Connection) ->
//! Result<..>` with a `tokio::sync::oneshot` for the reply; the thread opens the
//! connection (WAL, `synchronous=FULL`, `cache_size=-2000`, `foreign_keys=ON`),
//! runs `schema::migrate`, then loops. `Store` is `Clone` (cheap); dropping the last
//! clone stops the thread.
//!
//! One writer per database file per process: `Store::open` hands out handles to a
//! process-wide worker registry keyed by the canonical file path, so a second open
//! of the same file joins the running writer instead of opening a second
//! connection to it.

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

use crate::capabilities::{JournalMode, StorageCapabilities, StorageDialect, SynchronousMode};
use crate::queries::{
    AdmissionCursor, AttachAdmission, AttachAdmissionResult, MessageAdmission,
    MessageAdmissionPage, MessageAdmissionResult, MessagePage, MessageRow, SearchHit,
    SessionCursor, SessionFilter, SessionPage, TaskAdmission, TaskAdmissionResult,
};
use crate::{StoreError, queries, schema};

const QUEUE_CAPACITY: usize = 128;
const BUSY_TIMEOUT_MS: u64 = 5_000;
const CHECKPOINT_TIMEOUT_MS: u64 = 100;
const CHECKPOINT_ATTEMPTS: usize = 3;
const CHECKPOINT_BACKOFF_MS: u64 = 25;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 1_000;
const WAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;
const SQLITE_SYNCHRONOUS_FULL: i64 = 2;

type Task = Box<dyn FnOnce(&mut Connection) + Send>;

/// Per-worker instrumentation. Everything here is an atomic updated on the
/// request path — no poller, no background thread — so an idle daemon pays
/// nothing and a spike is recorded even if it lasts one call.
///
/// `wait` and `exec` are the pair that answers "is the store slow, or is it
/// just backed up": `exec` is the cost of the work itself (for a write, mostly
/// the fsync that `synchronous=FULL` asks for), `wait` is how long a caller
/// queued behind other callers.
#[derive(Debug, Default)]
struct StoreMetrics {
    wait: LatencyHist,
    exec: LatencyHist,
    enqueued: AtomicU64,
    completed: AtomicU64,
    /// Sends that found the queue closed or full.
    refused: AtomicU64,
    faults: AtomicU64,
    queue_depth: AtomicU64,
    queue_high_water: AtomicU64,
}

impl StoreMetrics {
    fn enqueued(&self) {
        self.enqueued.fetch_add(1, Ordering::Relaxed);
        let depth = self.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.queue_high_water.fetch_max(depth, Ordering::Relaxed);
    }

    fn dequeued(&self) {
        self.queue_depth.fetch_sub(1, Ordering::Relaxed);
        self.completed.fetch_add(1, Ordering::Relaxed);
    }
}

enum Command {
    Execute(Task),
    Close,
}

struct Inner {
    sender: mpsc::Sender<Command>,
    closing: AtomicBool,
    joined: watch::Receiver<bool>,
    joined_tx: watch::Sender<bool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    is_memory: bool,
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
            is_memory,
            shared_key: shared_key.clone(),
            fault: AtomicBool::new(false),
            metrics: Arc::new(StoreMetrics::default()),
        });

        let worker_inner = Arc::downgrade(&inner);
        let builder = std::thread::Builder::new().name("pacode-store".to_string());
        let handle = builder
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    Self::run_worker(path, is_memory, worker_inner, init_tx, rx);
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

        while let Some(command) = rx.blocking_recv() {
            match command {
                Command::Execute(task) => task(&mut conn),
                Command::Close => {
                    rx.close();
                    break;
                }
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
            PRAGMA synchronous = FULL;
            PRAGMA cache_size = -2000;
            PRAGMA foreign_keys = ON;
            PRAGMA wal_autocheckpoint = {WAL_AUTOCHECKPOINT_PAGES};
            PRAGMA journal_size_limit = {WAL_SIZE_LIMIT_BYTES};
            "#,
        ))?;
        let synchronous: i64 = conn.query_row("PRAGMA synchronous;", [], |row| row.get(0))?;
        if !is_memory && synchronous != SQLITE_SYNCHRONOUS_FULL {
            return Err(StoreError::Invalid(format!(
                "sqlite refused synchronous = FULL: synchronous = {synchronous}"
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

    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    ) -> impl std::future::Future<Output = Result<T, StoreError>> {
        let inner = self.inner.clone();
        async move {
            if inner.closing.load(Ordering::Acquire) {
                return Err(StoreError::Closed);
            }
            if inner.fault.load(Ordering::Acquire) {
                return Err(StoreError::Invalid("injected store failure".into()));
            }

            let (tx, rx) = oneshot::channel();
            let is_memory = inner.is_memory;
            let metrics = Arc::clone(&inner.metrics);
            // Stamped here, read inside the worker: the gap between the two is
            // exactly the time this caller spent queued behind other callers.
            let enqueued_at = std::time::Instant::now();
            let command = Command::Execute(Box::new(move |conn| {
                let started = std::time::Instant::now();
                metrics
                    .wait
                    .record_duration(started.duration_since(enqueued_at));
                metrics.dequeued();
                let result = match catch_unwind(AssertUnwindSafe(|| f(conn))) {
                    Ok(result) => {
                        if let Err(error) = &result {
                            let _ = catch_unwind(AssertUnwindSafe(|| {
                                Self::recover_error(conn, error, is_memory);
                            }));
                        }
                        result
                    }
                    Err(_) => {
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            Self::rollback_if_needed(conn);
                        }));
                        Err(StoreError::Invalid(
                            "store worker task panicked".to_string(),
                        ))
                    }
                };
                metrics.exec.record_duration(started.elapsed());
                if result.is_err() {
                    metrics.faults.fetch_add(1, Ordering::Relaxed);
                }
                let _ = tx.send(result);
            }));

            inner.metrics.enqueued();
            if inner.sender.send(command).await.is_err() {
                inner.metrics.dequeued();
                inner.metrics.refused.fetch_add(1, Ordering::Relaxed);
                return Err(StoreError::Closed);
            }
            rx.await.map_err(|_| StoreError::Closed)?
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
            queue_depth: metrics.queue_depth.load(Ordering::Relaxed),
            queue_high_water: metrics.queue_high_water.load(Ordering::Relaxed),
            wait: metrics.wait.snapshot(),
            exec: metrics.exec.snapshot(),
        }
    }

    fn is_recoverable_error(error: &StoreError) -> bool {
        matches!(error, StoreError::Sqlite(error) if Self::is_recoverable_sqlite(error))
    }

    fn is_recoverable_sqlite(error: &rusqlite::Error) -> bool {
        matches!(
            error.sqlite_error().map(|error| error.code),
            Some(rusqlite::ffi::ErrorCode::DatabaseBusy)
                | Some(rusqlite::ffi::ErrorCode::DatabaseLocked)
                | Some(rusqlite::ffi::ErrorCode::DiskFull)
        )
    }

    fn is_full_error(error: &StoreError) -> bool {
        matches!(
            error,
            StoreError::Sqlite(error)
                if matches!(
                    error.sqlite_error().map(|error| error.code),
                    Some(rusqlite::ffi::ErrorCode::DiskFull)
                )
        )
    }

    fn recover_error(conn: &mut Connection, error: &StoreError, is_memory: bool) {
        if !Self::is_recoverable_error(error) {
            return;
        }
        Self::rollback_if_needed(conn);
        if !is_memory {
            Self::checkpoint_wal(conn, Self::is_full_error(error));
        }
    }

    fn rollback_if_needed(conn: &mut Connection) {
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
    }

    fn checkpoint_wal(conn: &mut Connection, truncate: bool) -> bool {
        let sql = if truncate {
            "PRAGMA wal_checkpoint(TRUNCATE);"
        } else {
            "PRAGMA wal_checkpoint(PASSIVE);"
        };
        let _ = conn.busy_timeout(Duration::from_millis(CHECKPOINT_TIMEOUT_MS));
        let mut checkpointed = false;
        for attempt in 0..CHECKPOINT_ATTEMPTS {
            match conn.query_row(sql, [], |row| row.get::<_, i64>(0)) {
                Ok(0) => {
                    checkpointed = true;
                    break;
                }
                Ok(_) => {}
                Err(error) if Self::is_recoverable_sqlite(&error) => {}
                Err(_) => break,
            }
            if attempt + 1 < CHECKPOINT_ATTEMPTS {
                std::thread::sleep(Duration::from_millis(CHECKPOINT_BACKOFF_MS));
            }
        }
        let _ = conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS));
        checkpointed
    }

    fn finish_db(conn: &mut Connection, is_memory: bool) {
        if is_memory {
            return;
        }
        Self::rollback_if_needed(conn);
        if !Self::checkpoint_wal(conn, true) {
            Self::checkpoint_wal(conn, false);
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
mod worker_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use pacode_types::{Effort, Mode, ModelRoute, SessionId, SessionMeta};

    use super::{QUEUE_CAPACITY, Store, StoreError};
    use pacode_types::AgentId;

    /// The instrumentation is only worth anything if it moves with real work.
    /// A call that goes through the writer must show up in both histograms and
    /// in the queue counters.
    #[tokio::test]
    async fn store_stats_count_the_calls_that_went_through_the_writer() {
        let store = Store::open_in_memory().unwrap();
        let before = store.stats();
        assert_eq!(before.completed, 0, "a fresh worker has done nothing");

        for index in 0..5 {
            store
                .max_message_seq(&SessionId::new("s"), &AgentId::main())
                .await
                .unwrap();
            let _ = index;
        }

        let after = store.stats();
        assert!(
            after.completed >= 5,
            "every call must be counted, got {}",
            after.completed
        );
        assert_eq!(
            after.queue_depth, 0,
            "the queue must drain back to empty once the calls land"
        );
        assert!(after.queue_high_water >= 1);
        assert_eq!(after.faults, 0, "a successful call is not a fault");
        assert!(
            after.wait.count >= 5 && after.exec.count >= 5,
            "wait={} exec={} — both histograms must see every call",
            after.wait.count,
            after.exec.count
        );
        assert!(
            after.exec.max_micros > 0,
            "a real call takes measurable time"
        );
    }

    fn sample_meta(id: &str) -> SessionMeta {
        SessionMeta {
            id: SessionId::new(id),
            name: None,
            cwd: PathBuf::from("/tmp"),
            git_branch: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            model: ModelRoute::new("mock", "mock-model"),
            effort: Effort::Low,
            mode: Mode::Build,
            first_prompt: None,
            zdr: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_queue_applies_backpressure() {
        let store = Store::open_in_memory().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let first_store = store.clone();
        let first = tokio::spawn(async move {
            first_store
                .call(move |_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok::<(), StoreError>(())
                })
                .await
        });

        tokio::task::yield_now().await;
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();

        let mut queued = Vec::with_capacity(QUEUE_CAPACITY);
        for _ in 0..QUEUE_CAPACITY {
            let queued_store = store.clone();
            queued.push(tokio::spawn(async move {
                queued_store.call(|_| Ok::<(), StoreError>(())).await
            }));
        }

        // The store writer is a dedicated OS thread, so polling the queue from this
        // task only has to wait for the runtime to poll the senders.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while store.inner.sender.capacity() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(store.inner.sender.capacity(), 0);

        let extra_store = store.clone();
        let extra =
            tokio::spawn(async move { extra_store.call(|_| Ok::<(), StoreError>(())).await });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!extra.is_finished());

        release_tx.send(()).unwrap();
        first.await.unwrap().unwrap();
        for task in queued {
            task.await.unwrap().unwrap();
        }
        extra.await.unwrap().unwrap();
        store.close().await;
    }

    #[tokio::test]
    async fn worker_survives_task_panic() {
        let store = Store::open_in_memory().unwrap();
        let result = store
            .call(|_| -> Result<(), StoreError> { panic!("task panic") })
            .await;

        assert!(matches!(result, Err(StoreError::Invalid(_))));
        store
            .call(|conn| {
                let value: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
                assert_eq!(value, 1);
                Ok(())
            })
            .await
            .unwrap();
        store.close().await;
    }

    #[tokio::test]
    async fn one_file_gets_one_writer_and_memory_stores_stay_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-writer.db");
        let other_path = dir.path().join("other-writer.db");

        let first = Store::open(&path).unwrap();
        let second = Store::open(&path).unwrap();
        assert!(
            Arc::ptr_eq(&first.inner, &second.inner),
            "a second open of the same file must join the running writer"
        );

        let other = Store::open(&other_path).unwrap();
        assert!(!Arc::ptr_eq(&first.inner, &other.inner));

        let memory_first = Store::open_in_memory().unwrap();
        let memory_second = Store::open_in_memory().unwrap();
        assert!(!Arc::ptr_eq(&memory_first.inner, &memory_second.inner));
        assert!(!Arc::ptr_eq(&memory_first.inner, &first.inner));

        let meta = sample_meta("ses_shared");
        first.upsert_session(&meta).await.unwrap();
        assert_eq!(
            second.get_session(&meta.id).await.unwrap(),
            Some(meta.clone())
        );

        other.close().await;
        memory_first.close().await;
        memory_second.close().await;
        second.close().await;
        assert!(first.get_session(&meta.id).await.unwrap().is_some());
        first.close().await;
    }

    #[tokio::test]
    async fn a_reopened_file_store_does_not_inherit_a_closed_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reopen.db");
        let meta = sample_meta("ses_reopen");

        let first = Store::open(&path).unwrap();
        first.upsert_session(&meta).await.unwrap();
        first.close().await;

        let second = Store::open(&path).unwrap();
        assert_eq!(
            second.get_session(&meta.id).await.unwrap(),
            Some(meta.clone())
        );
        let updated = SessionMeta {
            updated_at_ms: meta.updated_at_ms + 7,
            ..meta
        };
        second.upsert_session(&updated).await.unwrap();
        assert_eq!(
            second.get_session(&updated.id).await.unwrap(),
            Some(updated)
        );
        second.close().await;
    }

    #[tokio::test]
    async fn a_file_store_meets_the_durability_contract_and_memory_reports_its_gap() {
        use crate::capabilities::{CapabilityGap, DialectRequirements};

        let dir = tempfile::tempdir().unwrap();
        let file_store = Store::open(&dir.path().join("durability.db")).unwrap();
        let capabilities = file_store.storage_capabilities().await.unwrap();
        assert_eq!(
            capabilities.gaps(&DialectRequirements::PACODE),
            Vec::<CapabilityGap>::new(),
            "a file store must satisfy the pacode storage contract"
        );
        file_store.close().await;

        let memory_store = Store::open_in_memory().unwrap();
        let memory_capabilities = memory_store.storage_capabilities().await.unwrap();
        assert_eq!(
            memory_capabilities.gaps(&DialectRequirements::PACODE),
            vec![CapabilityGap::WalFull]
        );
        memory_store.close().await;
    }
}
