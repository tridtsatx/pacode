//! Group commit: turning a queue of calls into as few commits as possible.
//!
//! The writer is one thread, so every call in the daemon queues behind it. A
//! commit is the only step that reaches the disk, which makes it the one cost
//! that must not be paid per call: a hundred agents writing at once would pay a
//! hundred fsyncs, and each of those fsyncs would sit in front of every other
//! caller while it happened.
//!
//! So the writer takes whatever has already queued up as one batch, runs it
//! inside one transaction with a savepoint per call, and commits once. Nobody is
//! told "done" until that commit is through, which is what makes a reply mean
//! "this is on disk" rather than "this is about to be".
//!
//! Three properties the grouping must not cost:
//!
//! - A call that fails is rolled back to its own savepoint and no further. The
//!   calls sharing its commit are none of its business.
//! - A call that panics is a failed call, not a dead writer.
//! - A commit that cannot happen is reported to every call in the batch, rather
//!   than leaving the ones that happened to share it believing otherwise.
//!
//! The one exception is a failure SQLite answers by rolling the whole
//! transaction back rather than the failing statement — busy, locked, out of
//! space. There the transaction is gone and nothing in the batch can be trusted,
//! so the batch is abandoned and every call in it is told.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::StoreError;
use crate::worker::{BUSY_TIMEOUT_MS, StoreMetrics};

/// The most calls one commit carries. Half the queue (`worker::QUEUE_CAPACITY`),
/// for the same reason: it
/// bounds how long the caller who arrived first waits behind calls it does not
/// care about, and it leaves the rest of the queue to fill the next batch while
/// this one commits. There is no timer anywhere in this — a batch is only ever
/// what has already arrived, so a quiet store commits one call at a time and pays
/// nothing for the grouping.
const MAX_BATCH_COMMANDS: usize = 64;

/// How long a checkpoint waits for the readers in its way before giving up. The
/// writer's own lock timeout is longer, and [`checkpoint_wal`] puts it back
/// afterwards: a checkpoint is worth waiting briefly for, a write is not.
const CHECKPOINT_TIMEOUT_MS: u64 = 100;
const CHECKPOINT_ATTEMPTS: usize = 3;
const CHECKPOINT_BACKOFF_MS: u64 = 25;

/// Savepoint per call inside a batch transaction. Reusing the name is fine and
/// free: SQLite resolves a repeated name to the most recent savepoint, and the
/// writer opens and closes one per call in stack order.
const SAVEPOINT_COMMAND: &str = "SAVEPOINT pacode_command;";
const RELEASE_COMMAND: &str = "RELEASE pacode_command;";
const ROLLBACK_COMMAND: &str = "ROLLBACK TO pacode_command; RELEASE pacode_command;";

/// The work of one call, with its answer type erased. The writer carries results
/// without knowing what each caller asked for, and the caller casts its own value
/// back out of the box it gets.
pub(crate) type Task =
    Box<dyn FnOnce(&mut Connection) -> Result<Box<dyn Any + Send>, StoreError> + Send>;
pub(crate) type Answer = Result<Box<dyn Any + Send>, StoreError>;
type Reply = oneshot::Sender<Answer>;

pub(crate) enum Command {
    Execute(Execution),
    Close,
}

/// One queued call: what to run, and the single slot its answer goes to.
pub(crate) struct Execution {
    /// Stamped by the caller. The gap to the writer picking it up is `wait`.
    pub(crate) enqueued_at: Instant,
    pub(crate) task: Task,
    pub(crate) reply: Reply,
}

/// The calls one commit will carry, in the order they were queued.
pub(crate) struct Batch {
    pub(crate) executions: Vec<Execution>,
    /// A [`Command::Close`] was in the queue: everything ahead of it ran, and the
    /// loop is done once this batch is committed.
    pub(crate) close_after: bool,
}

/// The transaction one commit will carry for every call in the batch.
fn begin_batch(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN DEFERRED;")
}

fn commit_batch(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch("COMMIT;")
}

fn abort_batch(conn: &mut Connection) {
    let _ = conn.execute_batch("ROLLBACK;");
}

fn savepoint_command(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SAVEPOINT_COMMAND)
}

fn release_command(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(RELEASE_COMMAND)
}

/// Undo one call's work and leave the batch open behind it.
fn rollback_command(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(ROLLBACK_COMMAND)
}

/// Take the first call, then everything else that has already queued up, and
/// stop at a [`Command::Close`] or at [`MAX_BATCH_COMMANDS`]. A call that
/// arrives after the drain belongs to the next batch rather than extending
/// this one: waiting for it would mean a timer in the writer, and a timer in
/// an idle daemon is exactly what this design refuses to pay for.
pub(crate) fn collect_batch(rx: &mut mpsc::Receiver<Command>, first: Command) -> Batch {
    let mut executions = Vec::new();
    let mut close_after = false;
    let mut next = first;
    loop {
        match next {
            Command::Execute(execution) => executions.push(execution),
            Command::Close => {
                close_after = true;
                break;
            }
        }
        if executions.len() >= MAX_BATCH_COMMANDS {
            break;
        }
        match rx.try_recv() {
            Ok(command) => next = command,
            Err(_) => break,
        }
    }
    Batch {
        executions,
        close_after,
    }
}

/// Run one batch, commit it once, and hand every caller its own answer, in
/// the order the calls were queued.
///
/// Each call inside the batch takes a savepoint, so a call that fails — a
/// constraint, a trigger, a collision — is rolled back on its own and the
/// calls behind it still commit. A call that panics is answered with an error
/// and rolled back the same way: one bad call must not take the writer, or
/// the batch it happens to share with twenty healthy calls, down with it.
pub(crate) fn run_batch(
    conn: &mut Connection,
    executions: Vec<Execution>,
    is_memory: bool,
    metrics: &StoreMetrics,
) {
    if executions.is_empty() {
        return;
    }
    metrics.batches.fetch_add(1, Ordering::Relaxed);

    // Picked up as a batch: these calls have left the queue, whatever
    // happens to them next.
    let picked = Instant::now();
    for execution in &executions {
        metrics
            .wait
            .record_duration(picked.duration_since(execution.enqueued_at));
        metrics.dequeued();
    }

    // One call is all a quiet store ever produces, and a lone statement is
    // already atomic: a transaction of its own would cost more than the
    // commit it saves. If the batch transaction cannot be opened at all, the
    // calls fall back to committing for themselves — slower, never a lie
    // about what reached the disk.
    let in_transaction = executions.len() > 1 && begin_batch(conn).is_ok();
    let mut answers: Vec<(Reply, Answer)> = Vec::with_capacity(executions.len());
    let mut abandon = false;
    let mut checkpoint = None;

    for execution in executions {
        let Execution { task, reply, .. } = execution;
        let started = Instant::now();
        let guarded = in_transaction && savepoint_command(conn).is_ok();
        let outcome = catch_unwind(AssertUnwindSafe(|| task(conn)));
        let result = match outcome {
            Ok(result) => result,
            Err(_) => Err(StoreError::Invalid(
                "store worker task panicked".to_string(),
            )),
        };
        if guarded {
            let _ = if result.is_ok() {
                release_command(conn)
            } else {
                rollback_command(conn)
            };
        }
        if let Err(error) = &result {
            if !in_transaction {
                // Autocommit: the call may have left a transaction of its own
                // open on the way out, exactly as it did before batching.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    rollback_if_needed(conn);
                }));
            }
            if is_recoverable_error(error) {
                abandon = true;
                checkpoint = Some(is_full_error(error));
            }
        }
        metrics.exec.record_duration(started.elapsed());
        answers.push((reply, result));
    }

    let committing = Instant::now();
    let lost = if !in_transaction {
        None
    } else if abandon {
        abort_batch(conn);
        Some(StoreError::Invalid(
            "store batch abandoned after a recoverable failure".to_string(),
        ))
    } else {
        match commit_batch(conn) {
            Ok(()) => None,
            Err(error) => {
                // A commit SQLite refuses — a deferred constraint, say — can
                // leave the transaction open, and the next batch must not
                // find one waiting for it.
                abort_batch(conn);
                Some(StoreError::Sqlite(error))
            }
        }
    };
    metrics.commit.record_duration(committing.elapsed());
    if let Some(truncate) = checkpoint
        && !is_memory
    {
        // Only now, with the transaction closed: a checkpoint taken inside an
        // open write transaction cannot make progress.
        checkpoint_wal(conn, truncate);
    }

    for (reply, result) in answers {
        // A batch that did not commit has written nothing. Answering "done"
        // to a call that was rolled back would be a lie the caller goes on to
        // act on, so the loss is reported instead.
        let result = match (lost.as_ref(), result) {
            (Some(lost), Ok(_)) => Err(StoreError::Invalid(format!(
                "store batch did not commit: {lost}"
            ))),
            (_, result) => result,
        };
        if result.is_err() {
            metrics.faults.fetch_add(1, Ordering::Relaxed);
        }
        let _ = reply.send(result);
    }
}

fn is_recoverable_error(error: &StoreError) -> bool {
    matches!(error, StoreError::Sqlite(error) if is_recoverable_sqlite(error))
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

/// Undo whatever a failed call left open on the connection.
///
/// A call inside a batch has already been rolled back to its own savepoint by
/// the batch runner, and this must not go further: rolling back here would
/// throw away the calls that shared the batch and had not failed at all. Only
/// the autocommit path — a lone call, or a batch that could not open its
/// transaction — can still have a transaction open here.
pub(crate) fn rollback_if_needed(conn: &mut Connection) {
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK;");
    }
}

pub(crate) fn checkpoint_wal(conn: &mut Connection, truncate: bool) -> bool {
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
            Err(error) if is_recoverable_sqlite(&error) => {}
            Err(_) => break,
        }
        if attempt + 1 < CHECKPOINT_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(CHECKPOINT_BACKOFF_MS));
        }
    }
    let _ = conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS));
    checkpointed
}
