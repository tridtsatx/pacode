//! Tests for the store writer: group commit, queue-depth gauge, `append_message_arc`
//! and the capped FTS text.

#![deny(warnings, clippy::all)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_store::Store;
use pacode_types::{AgentId, CallId, Effort, Message, Mode, ModelRoute, SessionId, SessionMeta};

fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: None,
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: ModelRoute::new("bubna", "gemini"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

/// Rows in the contentless FTS table, read through a second connection.
fn fts_rows(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT count(*) FROM messages_fts;", [], |r| r.get(0))
        .unwrap()
}

fn long_tool_output() -> String {
    format!("headword {} tailword", "x ".repeat(5000))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_depth_returns_to_zero() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.queue_depth(), 0);
    store.upsert_session(&meta("s")).await.unwrap();
    let mut handles = Vec::new();
    for i in 0..100u64 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .append_message(
                    &SessionId::new("s"),
                    &AgentId::main(),
                    i,
                    &Message::user("hello"),
                )
                .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }
    assert_eq!(store.queue_depth(), 0);
    store.close().await;
}

#[tokio::test]
async fn append_message_arc_round_trips() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("s");
    store.upsert_session(&meta("s")).await.unwrap();
    let msg = Arc::new(Message::user("shared payload"));
    store
        .append_message_arc(&session, &AgentId::main(), 1, Arc::clone(&msg))
        .await
        .unwrap();
    let rows = store
        .load_messages(&session, &AgentId::main())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message, *msg);
}

#[tokio::test]
async fn fts_indexes_only_head_of_tool_results_and_delete_is_symmetric() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fts.db");
    let store = Store::open(&path).unwrap();
    let session = SessionId::new("s");
    store.upsert_session(&meta("s")).await.unwrap();
    let msg = Message::tool_result(CallId::new("c1"), long_tool_output(), false);
    store
        .append_message(&session, &AgentId::main(), 1, &msg)
        .await
        .unwrap();
    // Replace the same seq: exercises the 'delete' + re-insert path.
    store
        .append_message(&session, &AgentId::main(), 1, &msg)
        .await
        .unwrap();

    assert_eq!(store.search("headword", 10).await.unwrap().len(), 1);
    assert!(store.search("tailword", 10).await.unwrap().is_empty());

    store.delete_session(&session).await.unwrap();
    assert!(store.search("headword", 10).await.unwrap().is_empty());
    store.close().await;
    assert_eq!(fts_rows(&path), 0);
}

#[tokio::test]
async fn open_rebuilds_legacy_full_text_fts_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let session = SessionId::new("s");
    {
        let store = Store::open(&path).unwrap();
        store.upsert_session(&meta("s")).await.unwrap();
        let msg = Message::tool_result(CallId::new("c1"), long_tool_output(), false);
        store
            .append_message(&session, &AgentId::main(), 1, &msg)
            .await
            .unwrap();
        store.close().await;
    }
    {
        // Emulate a v3 database: full tool output indexed, old user_version.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('delete-all');")
            .unwrap();
        conn.execute(
            "INSERT INTO messages_fts(rowid, text) SELECT 1, ?1;",
            [long_tool_output()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 3;").unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.search("headword", 10).await.unwrap().len(), 1);
    assert!(store.search("tailword", 10).await.unwrap().is_empty());
    store.delete_session(&session).await.unwrap();
    store.close().await;
    assert_eq!(fts_rows(&path), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_are_all_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many.db");
    let store = Store::open(&path).unwrap();
    store.upsert_session(&meta("s")).await.unwrap();

    let mut tasks = Vec::new();
    for t in 0..32u64 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            let agent = AgentId::new(format!("agent_{t}"));
            for i in 0..63u64 {
                store
                    .append_message(&SessionId::new("s"), &agent, i, &Message::user("word"))
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let mut total = 0;
    for t in 0..32u64 {
        let agent = AgentId::new(format!("agent_{t}"));
        total += store
            .load_messages(&SessionId::new("s"), &agent)
            .await
            .unwrap()
            .len();
    }
    assert_eq!(total, 32 * 63);
    assert_eq!(store.queue_depth(), 0);
    store.close().await;
    assert_eq!(fts_rows(&path), 32 * 63);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_command_does_not_lose_others_in_its_batch() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("s");
    store.upsert_session(&meta("s")).await.unwrap();
    let agent = AgentId::main();

    // Concurrent tasks, so many of these land in the same batch. One targets a missing
    // session (foreign-key violation) and must fail alone.
    let mut handles = Vec::new();
    for i in 0..10u64 {
        for target in ["s", "no_such_session"] {
            if target != "s" && i != 4 {
                continue;
            }
            let store = store.clone();
            let agent = agent.clone();
            let sid = SessionId::new(target);
            handles.push(tokio::spawn(async move {
                store
                    .append_message(&sid, &agent, i, &Message::user("ok"))
                    .await
            }));
        }
    }
    let mut failures = 0;
    for h in handles {
        if h.await.unwrap().is_err() {
            failures += 1;
        }
    }
    assert_eq!(failures, 1);
    let rows = store.load_messages(&session, &agent).await.unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(store.search("ok", 100).await.unwrap().len(), 10);
}
