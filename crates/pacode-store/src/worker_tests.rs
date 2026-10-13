//! Tests for the writer thread: the queue, the group commit inside it, and the
//! connection the commits go to.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Waker};

use pacode_types::AgentId;
use pacode_types::{Effort, Mode, ModelRoute, SessionId, SessionMeta};
use rusqlite::params;

use super::{QUEUE_CAPACITY, Store, StoreError};

const FIVE_SECONDS: std::time::Duration = std::time::Duration::from_secs(5);

/// A row every batching test writes, so "did this call actually land" is a
/// question the database answers rather than a question about counters.
const PROBE_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS batch_probe (v INTEGER PRIMARY KEY, tag TEXT NOT NULL);";

/// Parks the writer inside one call, so everything queued behind it is
/// waiting when the park is released — which is the only way to put several
/// calls in one batch on demand instead of hoping the scheduler does it.
struct Parked {
    release: std::sync::mpsc::SyncSender<()>,
    call: tokio::task::JoinHandle<Result<(), StoreError>>,
}

impl Parked {
    fn new(store: &Store) -> Self {
        let (parked_tx, parked_rx) = std::sync::mpsc::sync_channel(1);
        let (release, release_rx) = std::sync::mpsc::sync_channel(1);
        let blocking = store.clone();
        let call = tokio::spawn(async move {
            blocking
                .call(move |_| {
                    let _ = parked_tx.send(());
                    let _ = release_rx.recv();
                    Ok::<(), StoreError>(())
                })
                .await
        });
        parked_rx.recv_timeout(FIVE_SECONDS).unwrap();
        Self { release, call }
    }

    /// Lets the park through and waits for that call to be answered.
    async fn release(self) {
        let _ = self.release.send(());
        self.call.await.unwrap().unwrap();
    }
}

/// Waits for the writer to be exactly `depth` calls behind, so a test that
/// releases a park knows the calls it queued are in the same batch and not
/// spread over the next few commits.
async fn wait_for_queue_depth(store: &Store, depth: u64) {
    for _ in 0..100_000 {
        if store.stats().queue_depth == depth {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "the writer never reached a queue depth of {depth}, stuck at {}",
        store.stats().queue_depth
    );
}

/// Queues one call and hands the future back without waiting for it: polling
/// a call future once is the whole send, so doing it here — rather than
/// spawning a task per call — is what lets a test fix the order calls are
/// queued in. The writer runs a batch in the order it received it, so that
/// order is the one under test.
fn queue<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, StoreError> + Send + 'static,
) -> Pin<Box<dyn Future<Output = Result<T, StoreError>>>> {
    let mut call: Pin<Box<_>> = Box::pin(store.call(f));
    let mut context = Context::from_waker(Waker::noop());
    let queued = call.as_mut().poll(&mut context);
    assert!(
        queued.is_pending(),
        "a call with room in the queue must enqueue on its first poll"
    );
    call
}

/// The rows that landed, in the order the writer ran the calls in.
async fn probe_order(store: &Store) -> Vec<i64> {
    store
        .call(|conn| {
            let mut stmt = conn.prepare("SELECT v FROM batch_probe ORDER BY v;")?;
            let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            Ok(rows.collect::<Result<Vec<i64>, _>>()?)
        })
        .await
        .unwrap()
}

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
    assert_eq!(
        after.commit.count, after.batches,
        "every commit is one sample of the commit histogram, and nothing else is"
    );
}

/// The whole point of the grouping: calls that arrive together are committed
/// together. Without this the writer pays a commit per call, and with 128
/// agents writing at once that is the store deciding how fast the daemon can
/// think.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_that_queue_together_share_one_commit() {
    const CALLS: usize = 32;
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();
    let before = store.stats();

    let parked = Parked::new(&store);
    let mut calls = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        let writer = store.clone();
        calls.push(tokio::spawn(async move {
            writer
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO batch_probe (v, tag) VALUES (?1, 'queued');",
                        params![index as i64],
                    )?;
                    Ok::<(), StoreError>(())
                })
                .await
        }));
    }
    wait_for_queue_depth(&store, CALLS as u64).await;
    parked.release().await;
    for call in calls {
        call.await.unwrap().unwrap();
    }

    let after = store.stats();
    let calls_done = after.completed - before.completed;
    let commits = after.batches - before.batches;
    assert_eq!(calls_done, CALLS as u64 + 1, "the park and the calls");
    assert_eq!(
        commits, 2,
        "one commit for the parked call on its own, one for the {CALLS} behind it \
             — got {commits} commits for {calls_done} calls"
    );
    assert_eq!(probe_order(&store).await.len(), CALLS);
    store.close().await;
}

/// A call that fails is rolled back to its own savepoint and answered with
/// its error; the calls sharing its commit are none of its business and go on
/// to land. Without the savepoint the first constraint violation would take
/// every concurrent write in the daemon down with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_call_is_rolled_back_alone_while_its_batch_commits() {
    const CALLS: usize = 8;
    const FAILING: usize = 3;
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();

    let parked = Parked::new(&store);
    let mut calls = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        let writer = store.clone();
        calls.push(tokio::spawn(async move {
            writer
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO batch_probe (v, tag) VALUES (?1, 'landed');",
                        params![index as i64],
                    )?;
                    if index == FAILING {
                        // A second statement of the same call fails on a
                        // constraint: a statement-level failure, which is the
                        // kind that leaves the transaction open and leaves the
                        // call's own first row to undo. A failed admission
                        // looks exactly like this — the row that reserves the
                        // id lands, the message behind it does not.
                        conn.execute(
                            "INSERT INTO batch_probe (v, tag) VALUES (?1, NULL);",
                            params![index as i64],
                        )?;
                    }
                    Ok::<u64, StoreError>(index as u64)
                })
                .await
        }));
    }
    wait_for_queue_depth(&store, CALLS as u64).await;
    parked.release().await;

    let mut landed = Vec::new();
    let mut failures = 0;
    for (index, call) in calls.into_iter().enumerate() {
        match call.await.unwrap() {
            Ok(value) => {
                assert_eq!(value, index as u64, "a caller gets its own answer");
                landed.push(index as i64);
            }
            Err(error) => {
                assert_eq!(index, FAILING, "only the failing call may fail");
                assert!(error.to_string().contains("NOT NULL"), "{error}");
                failures += 1;
            }
        }
    }

    assert_eq!(failures, 1);
    landed.sort_unstable();
    assert_eq!(
        landed,
        (0..CALLS as i64)
            .filter(|index| *index != FAILING as i64)
            .collect::<Vec<_>>(),
        "every other call in the batch must still have committed"
    );
    assert_eq!(
        probe_order(&store).await,
        landed,
        "and it must be on disk, not just answered"
    );
    assert!(store.stats().faults >= 1, "a failed call is a fault");
    store.close().await;
}

/// A panic is a failed call, not a dead writer: the batch keeps the work of
/// the calls that did not panic, the caller's error is the one it has always
/// been, and the writer is still there afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_call_does_not_take_its_batch_or_the_writer_with_it() {
    const CALLS: usize = 6;
    const PANICKING: usize = 2;
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();

    let parked = Parked::new(&store);
    let mut calls = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        let writer = store.clone();
        calls.push(tokio::spawn(async move {
            writer
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO batch_probe (v, tag) VALUES (?1, 'before the panic');",
                        params![index as i64],
                    )?;
                    assert_ne!(index, PANICKING, "call {index} panics");
                    Ok::<u64, StoreError>(index as u64)
                })
                .await
        }));
    }
    wait_for_queue_depth(&store, CALLS as u64).await;
    parked.release().await;

    let mut landed = Vec::new();
    for (index, call) in calls.into_iter().enumerate() {
        match call.await.unwrap() {
            Ok(value) => {
                assert_eq!(value, index as u64);
                landed.push(index as i64);
            }
            Err(error) => {
                assert_eq!(index, PANICKING);
                assert!(error.to_string().contains("panicked"), "{error}");
            }
        }
    }
    landed.sort_unstable();
    assert_eq!(
        landed,
        (0..CALLS as i64)
            .filter(|index| *index != PANICKING as i64)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        probe_order(&store).await,
        landed,
        "the panicking call's own row has to be rolled back, and only its own"
    );

    // The writer is still a writer.
    store
        .call(|conn| {
            conn.execute("INSERT INTO batch_probe (v, tag) VALUES (99, 'after');", [])?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();
    assert!(probe_order(&store).await.contains(&99));
    store.close().await;
}

/// A batch is answered in the order it was queued, and each caller gets its
/// own answer rather than a neighbour's. The row order is what shows the
/// writer ran the calls in queue order; the returned values are what show
/// the answers were not crossed on the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_runs_and_answers_its_calls_in_the_order_they_were_queued() {
    const CALLS: usize = 12;
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();

    let parked = Parked::new(&store);
    let mut calls = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        let writer = store.clone();
        calls.push(queue(&writer, move |conn| {
            conn.execute(
                "INSERT INTO batch_probe (v, tag) VALUES (?1, 'in order');",
                params![index as i64],
            )?;
            Ok::<u64, StoreError>(index as u64)
        }));
    }
    parked.release().await;

    for (index, call) in calls.into_iter().enumerate() {
        assert_eq!(
            call.await.unwrap(),
            index as u64,
            "call {index} was answered with another call's value"
        );
    }
    assert_eq!(
        probe_order(&store).await,
        (0..CALLS as i64).collect::<Vec<_>>()
    );
    store.close().await;
}

/// The point of holding the replies back until the commit: a caller that is
/// told "done" is looking at a commit that already happened, shared with
/// every other call in its batch. A second connection is the only witness
/// that can tell the difference — the writer's own reads cannot, they see
/// uncommitted rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_arrives_after_the_commit_that_carried_the_whole_batch() {
    const CALLS: usize = 4;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commit-before-reply.db");
    let store = Store::open(&path).unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();

    let parked = Parked::new(&store);
    let mut calls = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        calls.push(queue(&store, move |conn| {
            conn.execute(
                "INSERT INTO batch_probe (v, tag) VALUES (?1, 'committed');",
                params![index as i64],
            )?;
            Ok::<(), StoreError>(())
        }));
    }
    wait_for_queue_depth(&store, CALLS as u64).await;
    parked.release().await;

    for call in calls {
        call.await.unwrap();
        // Every call of the batch, answered or not yet, is on disk by now.
        let witness = rusqlite::Connection::open(&path).unwrap();
        let committed: i64 = witness
            .query_row("SELECT COUNT(*) FROM batch_probe;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            committed, CALLS as i64,
            "a reply arrived before the commit that carried the batch"
        );
    }
    store.close().await;
}

/// A batch that cannot be committed has written nothing, so every caller in
/// it has to hear about it — including the ones whose own statements were
/// fine. A deferred foreign key is the deterministic way to fail a commit:
/// the statement is accepted, and the transaction is rejected at the end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_that_cannot_commit_tells_every_caller_in_it() {
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(
                "CREATE TABLE batch_parent (id INTEGER PRIMARY KEY);
                     CREATE TABLE batch_child (
                         id INTEGER PRIMARY KEY,
                         parent_id INTEGER REFERENCES batch_parent(id) DEFERRABLE INITIALLY DEFERRED
                     );",
            )?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();

    let parked = Parked::new(&store);
    let innocent = queue(&store, |conn| {
        conn.execute("INSERT INTO batch_parent (id) VALUES (1);", [])?;
        Ok::<(), StoreError>(())
    });
    let orphan = queue(&store, |conn| {
        conn.execute(
            "INSERT INTO batch_child (id, parent_id) VALUES (1, 404);",
            [],
        )?;
        Ok::<(), StoreError>(())
    });
    parked.release().await;

    let orphan = orphan.await.unwrap_err();
    assert!(orphan.to_string().contains("FOREIGN KEY"), "{orphan}");
    let innocent = innocent.await.unwrap_err();
    assert!(
        innocent.to_string().contains("did not commit"),
        "a caller must not be told its work is done when the commit carrying it \
             failed: {innocent}"
    );

    // Nothing landed, and the writer is still a writer: a rejected commit can
    // leave a transaction open, and one left open would take every batch
    // after it down with it.
    let parents: i64 = store
        .call(
            |conn| Ok(conn.query_row("SELECT COUNT(*) FROM batch_parent;", [], |row| row.get(0))?),
        )
        .await
        .unwrap();
    assert_eq!(parents, 0, "a batch that did not commit wrote nothing");
    store
        .call(|conn| {
            conn.execute("INSERT INTO batch_parent (id) VALUES (2);", [])?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();
    store.close().await;
}

/// A batch of one is what a quiet store always produces, and it must not pay
/// for the grouping machinery: no transaction of its own, no savepoint — which
/// is why a statement that fails half way through leaves the statements before
/// it committed, exactly as it did before there was a group commit.
#[tokio::test]
async fn a_lone_call_commits_itself_without_the_batch_machinery() {
    let store = Store::open_in_memory().unwrap();
    store
        .call(|conn| {
            conn.execute_batch(PROBE_TABLE)?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap();
    let before = store.stats();

    let error = store
        .call(|conn| {
            conn.execute("INSERT INTO batch_probe (v, tag) VALUES (1, 'kept');", [])?;
            conn.execute("INSERT INTO batch_probe (v, tag) VALUES (1, NULL);", [])?;
            Ok::<(), StoreError>(())
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("NOT NULL"), "{error}");

    let after = store.stats();
    assert_eq!(after.batches - before.batches, 1);
    assert_eq!(after.commit.count - before.commit.count, 1);
    // No transaction wrapped the call, so the first statement is committed
    // and only the second one is lost.
    let rows: Vec<(i64, String)> = store
        .call(|conn| {
            let mut stmt = conn.prepare("SELECT v, tag FROM batch_probe;")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows, vec![(1, "kept".to_string())]);
    store.close().await;
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
    let extra = tokio::spawn(async move { extra_store.call(|_| Ok::<(), StoreError>(())).await });
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
async fn a_file_store_meets_the_storage_contract_and_reports_its_durability() {
    use crate::capabilities::{CapabilityGap, DialectRequirements, SynchronousMode};

    let dir = tempfile::tempdir().unwrap();
    let file_store = Store::open(&dir.path().join("durability.db")).unwrap();
    let capabilities = file_store.storage_capabilities().await.unwrap();
    assert_eq!(
        capabilities.gaps(&DialectRequirements::PACODE),
        Vec::<CapabilityGap>::new(),
        "a file store must satisfy the pacode storage contract"
    );
    assert_eq!(
        capabilities.synchronous,
        SynchronousMode::Normal,
        "the writer must report the durability level it actually runs at, \
             not the one it used to ask for"
    );
    file_store.close().await;

    let memory_store = Store::open_in_memory().unwrap();
    let memory_capabilities = memory_store.storage_capabilities().await.unwrap();
    assert_eq!(
        memory_capabilities.gaps(&DialectRequirements::PACODE),
        vec![CapabilityGap::Wal],
        "a memory store cannot offer WAL, and must say so"
    );
    memory_store.close().await;
}

/// The stop origin and the spawn-time tool set are written by their own
/// calls only: the `AgentInfo` upserts every status change makes leave them
/// as they are.
#[tokio::test]
async fn a_subagents_restore_state_outlives_its_info_upserts() {
    let store = Store::open_in_memory().unwrap();
    let meta = sample_meta("ses_restore");
    store.upsert_session(&meta).await.unwrap();
    let mut info = pacode_types::AgentInfo {
        id: AgentId::new("agt_sub"),
        name: "sub".into(),
        kind: pacode_types::AgentKind::Sub,
        status: pacode_types::AgentStatus::Idle,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    let tools = vec!["read".to_string(), "report_status".to_string()];
    store
        .create_agent_with_history(&meta.id, &info, Some("brief"), &tools, &[])
        .await
        .unwrap();
    store
        .set_agent_stopped_by(&meta.id, &info.id, Some("user"))
        .await
        .unwrap();
    info.status = pacode_types::AgentStatus::Stopped;
    store.upsert_agent(&meta.id, &info, None).await.unwrap();

    let restore = store.agent_restore(&meta.id, &info.id).await.unwrap();
    assert_eq!(
        restore,
        crate::AgentRestore {
            prompt: Some("brief".into()),
            stopped_by: Some("user".into()),
            tools: Some(tools),
        }
    );
    store
        .set_agent_stopped_by(&meta.id, &info.id, None)
        .await
        .unwrap();
    let restore = store.agent_restore(&meta.id, &info.id).await.unwrap();
    assert_eq!(restore.stopped_by, None);
    assert_eq!(
        store
            .agent_restore(&meta.id, &AgentId::new("agt_missing"))
            .await
            .unwrap(),
        crate::AgentRestore::default()
    );
    store.close().await;
}
