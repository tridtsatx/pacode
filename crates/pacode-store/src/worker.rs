//! `Store`: async facade over a single SQLite writer thread.
//!
//! Implementation: `std::sync::mpsc` of boxed closures `FnOnce(&mut Connection) ->
//! Result<..>` with a `tokio::sync::oneshot` for the reply; the thread opens the
//! connection (WAL, `synchronous=NORMAL`, `cache_size=-2000`, `foreign_keys=ON`),
//! runs `schema::migrate`, then loops. `Store` is `Clone` (cheap); dropping the last
//! clone stops the thread.
//!
//! Group commit: the thread takes one command, then drains up to [`MAX_BATCH`] - 1 more
//! that are already queued (`try_recv`, never waits) and runs them all inside one
//! `BEGIN IMMEDIATE .. COMMIT`. Query functions that need atomicity use
//! `conn.savepoint()` (nest-safe), so a failing command rolls back only itself.
//! Replies are sent only after the COMMIT returns, so a caller is never acknowledged
//! for a write that is not yet committed. What a crash can lose: commands still in the
//! channel or in the open batch, none of which has been acknowledged (at most
//! `MAX_BATCH` commands plus whatever the queue holds). Acknowledged writes keep the
//! old guarantee: WAL + `synchronous=NORMAL` survive a process crash, while an OS crash
//! or power loss may lose the most recent commits (including acknowledged ones) back
//! to the last WAL checkpoint, exactly as before batching. Cost: an ack can wait for
//! up to `MAX_BATCH` commands ahead of it in the same batch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use pacode_types::{
    AgentId, AgentInfo, Message, Plan, SessionId, SessionMeta, TaskInfo, Usage, UsageTotals,
};
use rusqlite::Connection;

use crate::queries::{MessageRow, SearchHit, SessionFilter};
use crate::{StoreError, queries, schema};

/// Max commands per transaction (1 blocking recv + 63 `try_recv`).
const MAX_BATCH: usize = 64;

/// Sends the caller's reply. Called after the batch COMMIT with `Some(message)` when the
/// COMMIT failed (a successful result then becomes an error), `None` otherwise.
type Reply = Box<dyn FnOnce(Option<&str>) + Send>;
/// Runs the command on the writer thread and returns its deferred reply.
type Task = Box<dyn FnOnce(&mut Connection) -> Reply + Send>;

enum Command {
    Execute(Task),
    Stop,
}

#[derive(Clone)]
pub struct Store {
    sender: mpsc::Sender<Command>,
    handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Commands sent but not yet taken by the writer thread. Owner: `call` increments
    /// (before the send), the worker decrements when it dequeues. Bounded by the number
    /// of in-flight callers; read via [`Store::queue_depth`].
    depth: Arc<AtomicUsize>,
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

    fn start_worker(path: PathBuf, is_memory: bool) -> Result<Store, StoreError> {
        let (init_tx, init_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel::<Command>();
        let depth = Arc::new(AtomicUsize::new(0));
        let worker_depth = Arc::clone(&depth);

        let builder = std::thread::Builder::new().name("pacode-store".to_string());
        let handle = builder
            .spawn(move || {
                let conn_res = if is_memory {
                    Connection::open_in_memory()
                } else {
                    Connection::open(&path)
                };

                let mut conn = match conn_res {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(StoreError::Sqlite(e)));
                        return;
                    }
                };

                if let Err(e) = Self::init_db(&mut conn, is_memory) {
                    let _ = init_tx.send(Err(e));
                    return;
                }

                if init_tx.send(Ok(())).is_err() {
                    return;
                }

                run_loop(&mut conn, &rx, &worker_depth);
            })
            .map_err(StoreError::Io)?;

        init_rx.recv().map_err(|_| StoreError::Closed)??;

        Ok(Store {
            sender: tx,
            handle: Arc::new(Mutex::new(Some(handle))),
            depth,
        })
    }

    /// Number of commands queued for the writer thread and not yet picked up (a gauge,
    /// 0 when idle). Commands in the batch currently executing are not counted.
    pub fn queue_depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }

    fn init_db(conn: &mut Connection, is_memory: bool) -> Result<(), StoreError> {
        if !is_memory {
            conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        }
        conn.execute_batch(
            r#"
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -2000;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;
        schema::migrate(conn)?;
        Ok(())
    }

    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    ) -> impl std::future::Future<Output = Result<T, StoreError>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = Command::Execute(Box::new(move |conn| {
            let res = f(conn);
            let reply: Reply = Box::new(move |commit_err: Option<&str>| {
                let res = match (res, commit_err) {
                    (Ok(_), Some(msg)) => Err(commit_failure(msg)),
                    (other, _) => other,
                };
                let _ = tx.send(res);
            });
            reply
        }));

        // Increment before the send so the worker's decrement can never underflow.
        self.depth.fetch_add(1, Ordering::Relaxed);
        let send_res = self.sender.send(cmd);
        if send_res.is_err() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
        }

        async move {
            if send_res.is_err() {
                return Err(StoreError::Closed);
            }
            rx.await.map_err(|_| StoreError::Closed)?
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

    /// Same as [`Store::append_message`] but takes a shared message: no deep clone.
    pub async fn append_message_arc(
        &self,
        session: &SessionId,
        agent: &AgentId,
        seq: u64,
        msg: Arc<Message>,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::append_message(conn, &session, &agent, seq, &msg))
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

    pub async fn list_tasks(&self, session: &SessionId) -> Result<Vec<TaskInfo>, StoreError> {
        let session = session.clone();
        self.call(move |conn| queries::list_tasks(conn, &session))
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
        let session = session.clone();
        let agent = agent.clone();
        let usage = *usage;
        self.call(move |conn| queries::add_usage(conn, &session, &agent, turn, &usage, cost_usd))
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

    /// Persist a compaction run (summary and/or stats) in the `compaction` table.
    pub async fn record_compaction(
        &self,
        session: &SessionId,
        agent: &AgentId,
        record: queries::CompactionRecord,
    ) -> Result<(), StoreError> {
        let session = session.clone();
        let agent = agent.clone();
        self.call(move |conn| queries::record_compaction(conn, &session, &agent, &record))
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

    /// Flush and stop the writer thread.
    pub async fn close(self) {
        let _ = self.sender.send(Command::Stop);
        let handle = {
            let mut guard = self.handle.lock().ok();
            guard.as_mut().and_then(|g| g.take())
        };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

fn commit_failure(msg: &str) -> StoreError {
    StoreError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(msg.to_string()),
    ))
}

/// Writer-thread main loop: block for one command, drain what is already queued (up to
/// `MAX_BATCH`), run the batch in one transaction.
fn run_loop(conn: &mut Connection, rx: &mpsc::Receiver<Command>, depth: &AtomicUsize) {
    let mut stop = false;
    while !stop {
        let Ok(first) = rx.recv() else { break };
        let mut batch: Vec<Task> = Vec::with_capacity(MAX_BATCH);
        let mut next = Some(first);
        while let Some(cmd) = next.take() {
            match cmd {
                Command::Execute(task) => {
                    depth.fetch_sub(1, Ordering::Relaxed);
                    batch.push(task);
                }
                Command::Stop => {
                    stop = true;
                    break;
                }
            }
            if batch.len() >= MAX_BATCH {
                break;
            }
            next = rx.try_recv().ok();
        }
        run_batch(conn, batch);
    }
}

/// Run `batch` inside `BEGIN IMMEDIATE .. COMMIT`, then send every reply. If BEGIN fails
/// the tasks still run, each atomic through its own savepoint (the pre-batching
/// behaviour). If COMMIT fails the batch is rolled back and every reply that would have
/// been `Ok` becomes an error.
fn run_batch(conn: &mut Connection, batch: Vec<Task>) {
    if batch.is_empty() {
        return;
    }
    let in_tx = conn.execute_batch("BEGIN IMMEDIATE;").is_ok();
    let replies: Vec<Reply> = batch.into_iter().map(|task| task(conn)).collect();
    let mut commit_err: Option<String> = None;
    if in_tx && let Err(e) = conn.execute_batch("COMMIT;") {
        commit_err = Some(format!("batch commit failed: {e}"));
        if let Err(rb) = conn.execute_batch("ROLLBACK;") {
            log::warn!("pacode-store: rollback after failed commit: {rb}");
        }
    }
    for reply in replies {
        reply(commit_err.as_deref());
    }
}
