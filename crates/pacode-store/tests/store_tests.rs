//! Tests for `pacode-store`.

#![deny(warnings, clippy::all)]

use std::path::PathBuf;
use std::sync::Arc;

use pacode_store::queries::AttachAdmissionResult;
use pacode_store::queries::{self, AdmissionCursor, SessionCursor, TaskAdmissionResult};
use pacode_store::{MessageAdmissionResult, MessageAdmissionState, SessionFilter, Store};
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, CallId, ClientId, Effort, Message, Mode,
    ModelRoute, Plan, PlanItem, PlanStatus, ProgressSource, SessionId, SessionMeta, TaskId,
    TaskInfo, TaskProgress, TaskStatus, Usage,
};
use rusqlite::{Connection, params};
use tokio::sync::Barrier;

fn store_conn() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    pacode_store::schema::migrate(&mut conn).unwrap();
    conn
}

fn agent_info(id: AgentId, name: &str) -> AgentInfo {
    AgentInfo {
        id,
        name: name.to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Idle,
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
    }
}

fn sample_meta(id: &str, cwd: &str, updated_at: u64) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: Some(format!("Session {id}")),
        cwd: PathBuf::from(cwd),
        git_branch: Some("main".to_string()),
        created_at_ms: 1_000,
        updated_at_ms: updated_at,
        model: ModelRoute::new("bubna", "gemini-3.8-flash"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: Some("Напиши архитектуру persistence слоя".to_string()),
        zdr: false,
    }
}

#[tokio::test]
async fn round_trip_session_all_fields() {
    let store = Store::open_in_memory().unwrap();
    let meta = sample_meta("ses_1", "/workspace/project", 2_000);

    store.upsert_session(&meta).await.unwrap();

    let loaded = store
        .get_session(&meta.id)
        .await
        .unwrap()
        .expect("session must exist");

    assert_eq!(loaded, meta);
}

#[tokio::test]
async fn round_trip_messages_tool_result_and_hidden() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_msg");
    let agent = AgentId::main();

    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    // 1. Normal user message
    let msg1 = Message::user("Привет мир");
    store
        .append_message(&session, &agent, 1, &msg1)
        .await
        .unwrap();

    // 2. Hidden assistant message
    let mut msg2 = Message::assistant_text("Hidden thoughts");
    msg2.meta.hidden = true;
    store
        .append_message(&session, &agent, 2, &msg2)
        .await
        .unwrap();

    // 3. Tool result message
    let msg3 = Message::tool_result(CallId::new("call_1"), "command output content", false);
    store
        .append_message(&session, &agent, 3, &msg3)
        .await
        .unwrap();

    let rows = store.load_messages(&session, &agent).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].seq, 1);
    assert_eq!(rows[0].message, msg1);
    assert_eq!(rows[1].seq, 2);
    assert!(rows[1].message.meta.hidden);
    assert_eq!(rows[1].message, msg2);
    assert_eq!(rows[2].seq, 3);
    assert_eq!(rows[2].message, msg3);
}

#[tokio::test]
async fn round_trip_agents() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_agent");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let agent_id = AgentId::generate();
    let info = AgentInfo {
        id: agent_id.clone(),
        name: "sub-worker".to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: Some("reading cargo.toml".to_string()),
        started_at_ms: 10_000,
        finished_at_ms: None,
        tokens_in: 500,
        tokens_out: 250,
        model: ModelRoute::new("bubna", "gemini-3.8-flash"),
        effort: Effort::Max,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };

    store
        .upsert_agent(&session, &info, Some("system prompt here"))
        .await
        .unwrap();

    let agents = store.list_agents(&session).await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0], info);

    // A subagent blocked on `ask_parent` round-trips its own status.
    let mut waiting = info.clone();
    waiting.status = AgentStatus::WaitingParent;
    store.upsert_agent(&session, &waiting, None).await.unwrap();
    assert_eq!(
        store.list_agents(&session).await.unwrap()[0].status,
        AgentStatus::WaitingParent
    );

    // Update status to finished
    let mut updated = info.clone();
    updated.status = AgentStatus::Finished;
    updated.finished_at_ms = Some(12_000);
    updated.summary = Some("done successfully".to_string());
    store.upsert_agent(&session, &updated, None).await.unwrap();

    let agents = store.list_agents(&session).await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0], updated);
}

#[tokio::test]
async fn agent_identity_is_scoped_to_session() {
    let store = Store::open_in_memory().unwrap();
    let first = SessionId::new("ses_agent_first");
    let second = SessionId::new("ses_agent_second");
    store
        .upsert_session(&sample_meta(first.as_str(), "/tmp/one", 1))
        .await
        .unwrap();
    store
        .upsert_session(&sample_meta(second.as_str(), "/tmp/two", 1))
        .await
        .unwrap();
    let make_info = |name: &str| AgentInfo {
        id: AgentId::main(),
        name: name.into(),
        kind: AgentKind::Main,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: None,
        summary: None,
        error: None,
    };
    store
        .upsert_agent(&first, &make_info("first-main"), None)
        .await
        .unwrap();
    store
        .upsert_agent(&second, &make_info("second-main"), None)
        .await
        .unwrap();

    assert_eq!(
        store.list_agents(&first).await.unwrap()[0].name,
        "first-main"
    );
    assert_eq!(
        store.list_agents(&second).await.unwrap()[0].name,
        "second-main"
    );
}

#[tokio::test]
async fn round_trip_tasks_with_progress() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_task");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let task_id = TaskId::generate();
    let progress = TaskProgress {
        current: Some(42),
        total: Some(100),
        percent: Some(42.0),
        message: Some("compiling crates".to_string()),
        source: ProgressSource::Reported,
        updated_at_ms: 10_500,
    };

    let task = TaskInfo {
        id: task_id.clone(),
        session: session.clone(),
        owner: AgentId::main(),
        label: "cargo test".to_string(),
        command: "cargo test --all".to_string(),
        cwd: PathBuf::from("/workspace"),
        status: TaskStatus::Running,
        backgrounded: true,
        exit_code: None,
        started_at_ms: 10_000,
        ended_at_ms: None,
        progress: Some(progress),
        warnings: 2,
        errors: 0,
        output_path: PathBuf::from("/tmp/task.log"),
        output_bytes: 1024,
        acked: false,
    };

    store.upsert_task(&task).await.unwrap();

    let tasks = store.list_tasks(&session).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0], task);
}

#[tokio::test]
async fn round_trip_plan() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_plan");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    // Default when missing
    let loaded_default = store.load_plan(&session).await.unwrap();
    assert_eq!(loaded_default, Plan::default());

    let plan = Plan {
        version: 1,
        items: vec![
            PlanItem {
                id: "1".to_string(),
                content: "Step 1".to_string(),
                status: PlanStatus::Done,
                progress: None,
            },
            PlanItem {
                id: "2".to_string(),
                content: "Step 2".to_string(),
                status: PlanStatus::Active,
                progress: Some(50),
            },
        ],
    };

    store.save_plan(&session, &plan).await.unwrap();
    let loaded = store.load_plan(&session).await.unwrap();
    assert_eq!(loaded, plan);
}

#[tokio::test]
async fn round_trip_usage_totals_with_and_without_cost() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_usage");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let agent = AgentId::main();

    // 1. Without cost
    let u1 = Usage {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 10,
        cache_read_tokens: 20,
        cache_write_tokens: 5,
    };
    store
        .add_usage(&session, &agent, 1, &u1, None)
        .await
        .unwrap();

    let totals1 = store.usage_totals(&session).await.unwrap();
    assert_eq!(totals1.input, 100);
    assert_eq!(totals1.output, 50);
    assert_eq!(totals1.reasoning, 10);
    assert_eq!(totals1.cache_read, 20);
    assert_eq!(totals1.cache_write, 5);
    assert_eq!(totals1.turns, 1);
    assert_eq!(totals1.cost_usd, None);

    // 2. With cost
    let u2 = Usage {
        input_tokens: 200,
        output_tokens: 100,
        reasoning_tokens: 20,
        cache_read_tokens: 40,
        cache_write_tokens: 10,
    };
    store
        .add_usage(&session, &agent, 2, &u2, Some(0.015))
        .await
        .unwrap();

    let totals2 = store.usage_totals(&session).await.unwrap();
    assert_eq!(totals2.input, 300);
    assert_eq!(totals2.output, 150);
    assert_eq!(totals2.turns, 2);
    assert!(totals2.cost_usd.is_some());
    let cost = totals2.cost_usd.unwrap();
    assert!((cost - 0.015).abs() < 1e-6);
}

#[tokio::test]
async fn round_trip_compaction() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_compact");
    let agent = AgentId::main();
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let non_existent = store.load_compaction(&session, &agent).await.unwrap();
    assert_eq!(non_existent, None);

    store
        .save_compaction(&session, &agent, "Summary of previous turns", 42)
        .await
        .unwrap();

    let loaded = store
        .load_compaction(&session, &agent)
        .await
        .unwrap()
        .expect("compaction must exist");
    assert_eq!(loaded, ("Summary of previous turns".to_string(), 42));
}

#[tokio::test]
async fn list_sessions_cwd_filter_and_ordering() {
    let store = Store::open_in_memory().unwrap();

    let s1 = sample_meta("ses_a", "/home/user/repo-a", 100);
    let s2 = sample_meta("ses_b", "/home/user/repo-b", 300);
    let s3 = sample_meta("ses_c", "/home/user/repo-a", 200);

    store.upsert_session(&s1).await.unwrap();
    store.upsert_session(&s2).await.unwrap();
    store.upsert_session(&s3).await.unwrap();

    // All sessions, newest updated first: s2 (300), s3 (200), s1 (100)
    let all = store.list_sessions(SessionFilter::default()).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, s2.id);
    assert_eq!(all[1].id, s3.id);
    assert_eq!(all[2].id, s1.id);

    // Filter by cwd: repo-a only -> s3 (200), s1 (100)
    let filtered = store
        .list_sessions(SessionFilter {
            cwd: Some(PathBuf::from("/home/user/repo-a")),
            limit: 0,
        })
        .await
        .unwrap();
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].id, s3.id);
    assert_eq!(filtered[1].id, s1.id);

    // Limit 1
    let limited = store
        .list_sessions(SessionFilter {
            cwd: None,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, s2.id);
}

#[tokio::test]
async fn load_messages_before_paging() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_page");
    let agent = AgentId::main();
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    for seq in 1..=10 {
        let msg = Message::user(format!("Message {seq}"));
        store
            .append_message(&session, &agent, seq, &msg)
            .await
            .unwrap();
    }

    // Last 3 messages (before is None, limit 3) -> ascending [8, 9, 10]
    let page1 = store
        .load_messages_before(&session, &agent, None, 3)
        .await
        .unwrap();
    let seqs1: Vec<u64> = page1.iter().map(|m| m.seq).collect();
    assert_eq!(seqs1, vec![8, 9, 10]);

    // Messages before seq 8, limit 3 -> [5, 6, 7]
    let page2 = store
        .load_messages_before(&session, &agent, Some(8), 3)
        .await
        .unwrap();
    let seqs2: Vec<u64> = page2.iter().map(|m| m.seq).collect();
    assert_eq!(seqs2, vec![5, 6, 7]);

    // Messages before seq 5, limit 10 -> [1, 2, 3, 4]
    let page3 = store
        .load_messages_before(&session, &agent, Some(5), 10)
        .await
        .unwrap();
    let seqs3: Vec<u64> = page3.iter().map(|m| m.seq).collect();
    assert_eq!(seqs3, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn delete_cascades_and_search_no_longer_finds_text() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_del");
    let agent = AgentId::main();
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let msg = Message::user("ЭксклюзивныйКлюч secret_token_xyz");
    store
        .append_message(&session, &agent, 1, &msg)
        .await
        .unwrap();

    let agent_info = AgentInfo {
        id: AgentId::generate(),
        name: "worker".to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 100,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("bubna", "gemini"),
        effort: Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    };
    store
        .upsert_agent(&session, &agent_info, None)
        .await
        .unwrap();

    // Verify search finds the secret text
    let hits_before = store.search("secret_token_xyz", 10).await.unwrap();
    assert_eq!(hits_before.len(), 1);
    assert_eq!(hits_before[0].session, session);

    // Delete session
    store.delete_session(&session).await.unwrap();

    // Verify everything owned by session is gone
    assert_eq!(store.get_session(&session).await.unwrap(), None);
    assert!(
        store
            .load_messages(&session, &agent)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.list_agents(&session).await.unwrap().is_empty());
    assert_eq!(store.load_plan(&session).await.unwrap(), Plan::default());

    // Search no longer finds the text
    let hits_after = store.search("secret_token_xyz", 10).await.unwrap();
    assert!(hits_after.is_empty());
}

#[tokio::test]
async fn fts_search_finds_cyrillic_and_english() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_fts");
    let agent = AgentId::main();
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();

    let msg = Message::user(
        "Разработка надежной системы persistence на Rust с поддержкой полнотекстового поиска FTS5",
    );
    store
        .append_message(&session, &agent, 1, &msg)
        .await
        .unwrap();

    // Cyrillic search
    let cyr_hits = store.search("Разработка", 5).await.unwrap();
    assert_eq!(cyr_hits.len(), 1);
    assert_eq!(cyr_hits[0].seq, 1);
    assert!(cyr_hits[0].snippet.contains("[Разработка]"));

    // English search
    let eng_hits = store.search("persistence", 5).await.unwrap();
    assert_eq!(eng_hits.len(), 1);
    assert_eq!(eng_hits[0].seq, 1);
    assert!(eng_hits[0].snippet.contains("[persistence]"));
}

#[tokio::test]
async fn store_clone_shares_thread() {
    let store1 = Store::open_in_memory().unwrap();
    let store2 = store1.clone();

    let meta = sample_meta("ses_clone", "/tmp", 1_000);
    store1.upsert_session(&meta).await.unwrap();

    // Read through clone 2
    let loaded = store2.get_session(&meta.id).await.unwrap();
    assert_eq!(loaded, Some(meta));

    // Dropping clone 1 leaves clone 2 functioning
    drop(store1);
    let loaded2 = store2
        .get_session(&SessionId::new("ses_clone"))
        .await
        .unwrap();
    assert!(loaded2.is_some());

    // Closing store2 shuts down the thread
    store2.close().await;
}

#[tokio::test]
async fn file_persistence_with_tempfile_creates_parent_dir() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir
        .path()
        .join("nested")
        .join("subfolder")
        .join("test.db");

    // Parent dir nested/subfolder does not exist yet; Store::open must create it
    {
        let store = Store::open(&db_path).unwrap();
        let meta = sample_meta("ses_file", "/workspace", 5_000);
        store.upsert_session(&meta).await.unwrap();

        let msg = Message::user("Persistent file message");
        store
            .append_message(&meta.id, &AgentId::main(), 1, &msg)
            .await
            .unwrap();

        store.close().await;
    }

    // Reopen from disk in a new store instance
    {
        let store = Store::open(&db_path).unwrap();
        let loaded_session = store
            .get_session(&SessionId::new("ses_file"))
            .await
            .unwrap();
        assert!(loaded_session.is_some());

        let messages = store
            .load_messages(&SessionId::new("ses_file"), &AgentId::main())
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message.text(), "Persistent file message");

        store.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn w128_admissions_are_atomic_and_ordered_across_restart() {
    const W128: usize = 128;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("w128-admissions.db");
    let session = SessionId::new("ses_w128_admissions");
    {
        let store = Store::open(&db_path).unwrap();
        store
            .upsert_session(&sample_meta(session.as_str(), "/tmp", 1_000))
            .await
            .unwrap();
        let message = Message::user("admission");
        let barrier = Arc::new(Barrier::new(W128));
        let mut tasks = Vec::with_capacity(W128);
        for seq in 0..W128 {
            let store = store.clone();
            let session = session.clone();
            let barrier = barrier.clone();
            let message = message.clone();
            let message_id = format!("w128-{seq}");
            let turn_id = format!("trn-{seq}");
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                store
                    .append_user_message_with_turn(
                        &session,
                        &AgentId::main(),
                        seq as u64,
                        &message_id,
                        seq as u64 + 1,
                        Some(&turn_id),
                        &message,
                    )
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), MessageAdmissionResult::Appended);
        }
        for seq in (0..W128).step_by(2) {
            assert!(
                store
                    .mark_message_admission_terminal(
                        &session,
                        &format!("w128-{seq}"),
                        Some("completed"),
                    )
                    .await
                    .unwrap()
            );
        }
        for seq in (1..W128).step_by(2) {
            let message_id = format!("w128-{seq}");
            let turn_id = format!("trn-{seq}");
            assert!(
                store
                    .mark_message_admission_running(&session, &message_id, &turn_id)
                    .await
                    .unwrap()
            );
        }
        store.close().await;
    }

    let store = Store::open(&db_path).unwrap();
    let admissions = store.load_message_admissions().await.unwrap();
    assert_eq!(admissions.len(), W128);
    assert_eq!(
        admissions
            .iter()
            .map(|admission| admission.message_seq.unwrap())
            .collect::<Vec<_>>(),
        (0..W128 as u64).collect::<Vec<_>>()
    );
    for (index, admission) in admissions.iter().enumerate() {
        assert_eq!(
            admission.state,
            if index % 2 == 0 {
                MessageAdmissionState::Terminal
            } else {
                MessageAdmissionState::Running
            }
        );
    }
    assert_eq!(
        store
            .load_messages(&session, &AgentId::main())
            .await
            .unwrap()
            .len(),
        W128
    );
    store.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w128_million_row_lazy_resume_pages_after_restart() {
    const W128: usize = 128;
    const ROWS: usize = 1_000_000;
    const PAGE: u32 = W128 as u32;
    const LOAD_CHUNK: usize = 10_000;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("w128-million.db");
    let session = SessionId::new("ses_w128_million");
    {
        let store = Store::open(&db_path).unwrap();
        store
            .upsert_session(&sample_meta(session.as_str(), "/tmp", 1_000))
            .await
            .unwrap();
        store.close().await;
    }

    let mut connection = Connection::open(&db_path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
        .unwrap();
    let message = serde_json::to_string(&Message::user("row")).unwrap();
    let agent = AgentId::main();
    for chunk_start in (0..ROWS).step_by(LOAD_CHUNK) {
        let transaction = connection.transaction().unwrap();
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO messages (session_id, agent_id, seq, role, content_json, hidden, created_at) VALUES (?1, ?2, ?3, 'user', ?4, 0, ?5)",
                )
                .unwrap();
            for seq in chunk_start..(chunk_start + LOAD_CHUNK).min(ROWS) {
                statement
                    .execute(params![
                        session.as_str(),
                        agent.as_str(),
                        seq as i64,
                        message.as_str(),
                        1_i64,
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
    }
    assert_eq!(indexed_row_count(&connection), 0);
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, ROWS as i64);
    drop(connection);

    let store = Store::open(&db_path).unwrap();
    let latest = store
        .load_messages_before(&session, &agent, None, PAGE)
        .await
        .unwrap();
    assert_eq!(latest.len(), PAGE as usize);
    assert_eq!(latest.first().unwrap().seq, ROWS as u64 - PAGE as u64);
    assert_eq!(latest.last().unwrap().seq, ROWS as u64 - 1);

    let mut before = latest.first().unwrap().seq;
    let mut oldest = before;
    for _ in 1..W128 {
        let page = store
            .load_messages_before(&session, &agent, Some(before), PAGE)
            .await
            .unwrap();
        assert_eq!(page.len(), PAGE as usize);
        let first = page.first().unwrap().seq;
        assert_eq!(first + PAGE as u64, before);
        oldest = first;
        before = first;
    }
    assert_eq!(oldest, ROWS as u64 - W128 as u64 * PAGE as u64);
    store.close().await;
}

#[tokio::test]
async fn message_admission_is_atomic_and_survives_store_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission.db");
    let session = SessionId::new("ses_admission_restart");
    let agent = AgentId::main();
    let message = Message::user("durable prompt");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);

    {
        let store = Store::open(&db_path).unwrap();
        store.upsert_session(&meta).await.unwrap();
        assert_eq!(
            store
                .append_user_message(&session, &agent, 0, "msg-1", 11, &message)
                .await
                .unwrap(),
            MessageAdmissionResult::Appended
        );
        assert_eq!(
            store
                .append_user_message(&session, &agent, 1, "msg-1", 11, &message)
                .await
                .unwrap(),
            MessageAdmissionResult::Duplicate
        );
        assert_eq!(
            store
                .append_user_message(&session, &agent, 2, "msg-1", 12, &message)
                .await
                .unwrap(),
            MessageAdmissionResult::Conflict
        );
        assert_eq!(
            store.load_messages(&session, &agent).await.unwrap().len(),
            1
        );
        let admissions = store.load_message_admissions().await.unwrap();
        assert_eq!(admissions.len(), 1);
        assert_eq!(admissions[0].session, session);
        assert_eq!(admissions[0].message_id, "msg-1");
        assert_eq!(admissions[0].fingerprint, 11);
        assert_eq!(
            store
                .get_message_admission(&session, "msg-1")
                .await
                .unwrap(),
            Some(admissions[0].clone())
        );
        store.close().await;
    }

    let store = Store::open(&db_path).unwrap();
    assert_eq!(
        store
            .append_user_message(&session, &agent, 3, "msg-1", 11, &message)
            .await
            .unwrap(),
        MessageAdmissionResult::Duplicate
    );
    assert_eq!(
        store.load_messages(&session, &agent).await.unwrap().len(),
        1
    );
    assert_eq!(store.load_message_admissions().await.unwrap().len(), 1);
    store.close().await;
}

#[tokio::test]
async fn message_admission_key_is_scoped_to_session() {
    let store = Store::open_in_memory().unwrap();
    let first = SessionId::new("ses_admission_first");
    let second = SessionId::new("ses_admission_second");
    store
        .upsert_session(&sample_meta(first.as_str(), "/tmp", 1))
        .await
        .unwrap();
    store
        .upsert_session(&sample_meta(second.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let message = Message::user("same id in two sessions");

    assert_eq!(
        store
            .append_user_message(&first, &AgentId::main(), 0, "shared-id", 23, &message)
            .await
            .unwrap(),
        MessageAdmissionResult::Appended
    );
    assert_eq!(
        store
            .append_user_message(&second, &AgentId::main(), 0, "shared-id", 23, &message)
            .await
            .unwrap(),
        MessageAdmissionResult::Appended
    );
    assert_eq!(store.load_message_admissions().await.unwrap().len(), 2);
}

#[tokio::test]
async fn failed_message_admission_append_leaves_no_durable_record() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission-failure.db");
    let store = Store::open(&db_path).unwrap();
    let session = SessionId::new("ses_admission_failure");
    let meta = sample_meta(session.as_str(), "/tmp", 1_000);
    store.upsert_session(&meta).await.unwrap();
    let trigger = Connection::open(&db_path).unwrap();
    trigger
        .execute_batch(
            "CREATE TRIGGER reject_message_append BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT, 'forced append failure'); END;",
        )
        .unwrap();
    let message = Message::user("must roll back");

    let error = store
        .append_user_message(&session, &AgentId::main(), 0, "msg-failed", 17, &message)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("forced append failure"));
    assert!(store.load_message_admissions().await.unwrap().is_empty());
    assert!(
        store
            .load_messages(&session, &AgentId::main())
            .await
            .unwrap()
            .is_empty()
    );
    drop(trigger);
    store.close().await;
}

#[tokio::test]
async fn message_admission_state_transitions_are_durable() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_admission_state");
    store
        .upsert_session(&sample_meta(session.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let message = Message::user("durable state");
    assert_eq!(
        store
            .append_user_message_with_turn(
                &session,
                &AgentId::main(),
                0,
                "msg-state",
                9,
                Some("trn_state"),
                &message,
            )
            .await
            .unwrap(),
        MessageAdmissionResult::Appended
    );
    assert!(
        store
            .mark_message_admission_running(&session, "msg-state", "trn_state")
            .await
            .unwrap()
    );
    assert!(
        !store
            .mark_message_admission_running(&session, "msg-state", "trn_state")
            .await
            .unwrap()
    );
    assert!(
        store
            .mark_message_admission_terminal(&session, "msg-state", Some("completed"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .mark_message_admission_terminal(&session, "msg-state", Some("failed"))
            .await
            .unwrap()
    );
    let admission = store
        .get_message_admission(&session, "msg-state")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admission.state, MessageAdmissionState::Terminal);
    assert_eq!(admission.message_seq, Some(0));
    assert_eq!(
        admission.turn_id.as_ref().map(|id| id.as_str()),
        Some("trn_state")
    );
    assert_eq!(admission.terminal_stop.as_deref(), Some("completed"));
}

#[tokio::test]
async fn usage_turn_id_makes_ledger_increment_idempotent() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_usage_idempotent");
    store
        .upsert_session(&sample_meta(session.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 3,
        ..Usage::default()
    };
    assert!(
        store
            .add_usage_for_turn(
                &session,
                &AgentId::main(),
                1,
                Some("trn_once"),
                &usage,
                None,
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .add_usage_for_turn(
                &session,
                &AgentId::main(),
                1,
                Some("trn_once"),
                &usage,
                None,
            )
            .await
            .unwrap()
    );
    let totals = store.usage_totals(&session).await.unwrap();
    assert_eq!(totals.input, 10);
    assert_eq!(totals.output, 3);
    assert_eq!(totals.turns, 1);
}

#[tokio::test]
async fn task_owner_reassignment_is_scoped_to_session() {
    let store = Store::open_in_memory().unwrap();
    let first = SessionId::new("ses_owner_first");
    let second = SessionId::new("ses_owner_second");
    store
        .upsert_session(&sample_meta(first.as_str(), "/tmp", 1))
        .await
        .unwrap();
    store
        .upsert_session(&sample_meta(second.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let make_task = |session: &SessionId| TaskInfo {
        id: TaskId::generate(),
        session: session.clone(),
        owner: AgentId::main(),
        label: "task".into(),
        command: "true".into(),
        cwd: PathBuf::from("/tmp"),
        status: TaskStatus::Running,
        backgrounded: false,
        exit_code: None,
        started_at_ms: 1,
        ended_at_ms: None,
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: PathBuf::from("/tmp/task.log"),
        output_bytes: 0,
        acked: false,
    };
    let first_task = make_task(&first);
    let second_task = make_task(&second);
    store.upsert_task(&first_task).await.unwrap();
    store.upsert_task(&second_task).await.unwrap();
    let new_owner = AgentId::generate();
    let changed = store
        .reassign_task_owner(&first, &AgentId::main(), &new_owner)
        .await
        .unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(store.list_tasks(&first).await.unwrap()[0].owner, new_owner);
    assert_eq!(
        store.list_tasks(&second).await.unwrap()[0].owner,
        AgentId::main()
    );
}

#[tokio::test]
async fn task_upsert_is_fenced_by_terminal_status_and_owner() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_task_fence");
    store
        .upsert_session(&sample_meta(session.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let task = TaskInfo {
        id: TaskId::generate(),
        session: session.clone(),
        owner: AgentId::main(),
        label: "task".into(),
        command: "true".into(),
        cwd: PathBuf::from("/tmp"),
        status: TaskStatus::Running,
        backgrounded: false,
        exit_code: None,
        started_at_ms: 1,
        ended_at_ms: None,
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: PathBuf::from("/tmp/task.log"),
        output_bytes: 0,
        acked: false,
    };
    store.upsert_task(&task).await.unwrap();

    let new_owner = AgentId::generate();
    store
        .reassign_task_owner(&session, &AgentId::main(), &new_owner)
        .await
        .unwrap();
    let reassigned = TaskInfo {
        owner: new_owner.clone(),
        ..task.clone()
    };
    let mut late_old_owner = task.clone();
    late_old_owner.status = TaskStatus::Failed;
    assert!(!store.upsert_task_checked(&late_old_owner).await.unwrap());
    let stored = store.list_tasks(&session).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].owner, reassigned.owner);
    assert_eq!(stored[0].status, TaskStatus::Running);

    let mut terminal = reassigned.clone();
    terminal.status = TaskStatus::Completed;
    terminal.ended_at_ms = Some(2);
    store.upsert_task(&terminal).await.unwrap();
    let mut late_running = terminal.clone();
    late_running.status = TaskStatus::Running;
    late_running.ended_at_ms = None;
    assert!(!store.upsert_task_checked(&late_running).await.unwrap());
    let stored = store.list_tasks(&session).await.unwrap();
    assert_eq!(stored[0].status, TaskStatus::Completed);
    assert_eq!(stored[0].owner, reassigned.owner);
}

#[tokio::test]
async fn detach_moves_messages_and_rollback_restores_owner() {
    let store = Store::open_in_memory().unwrap();
    let session = SessionId::new("ses_detach_rollback");
    store
        .upsert_session(&sample_meta(session.as_str(), "/tmp", 1))
        .await
        .unwrap();
    let old_owner = AgentId::main();
    let new_owner = AgentId::generate();
    let make_info = |id: AgentId, name: &str| AgentInfo {
        id,
        name: name.into(),
        kind: if name == "main" {
            AgentKind::Main
        } else {
            AgentKind::Sub
        },
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: if name == "main" {
            None
        } else {
            Some(AgentId::main())
        },
        summary: None,
        error: None,
    };
    store
        .upsert_agent(&session, &make_info(old_owner.clone(), "main"), None)
        .await
        .unwrap();
    store
        .upsert_agent(&session, &make_info(new_owner.clone(), "detached"), None)
        .await
        .unwrap();
    let prior_message = Message::user("prior message");
    store
        .append_message(&session, &old_owner, 0, &prior_message)
        .await
        .unwrap();
    let message = Message::user("turn message");
    store
        .append_user_message_with_turn(
            &session,
            &old_owner,
            1,
            "detach-message",
            1,
            Some("trn_detach"),
            &message,
        )
        .await
        .unwrap();
    let rows = vec![(0, Arc::new(prior_message)), (1, Arc::new(message))];
    store
        .save_compaction(&session, &old_owner, "detached summary", 0)
        .await
        .unwrap();
    store
        .detach_agent(
            &session,
            &make_info(new_owner.clone(), "detached"),
            None,
            &rows,
            &[0],
            None,
            &old_owner,
            &new_owner,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .load_messages(&session, &old_owner)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .load_messages(&session, &new_owner)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .get_message_admission(&session, "detach-message")
            .await
            .unwrap()
            .unwrap()
            .agent,
        new_owner
    );
    assert_eq!(
        store.load_compaction(&session, &old_owner).await.unwrap(),
        Some(("detached summary".into(), 0))
    );
    assert_eq!(
        store.load_compaction(&session, &new_owner).await.unwrap(),
        Some(("detached summary".into(), 0))
    );

    store
        .rollback_detach(&session, &new_owner, &rows, &old_owner)
        .await
        .unwrap();
    assert_eq!(
        store
            .load_messages(&session, &old_owner)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        store
            .load_messages(&session, &new_owner)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .get_message_admission(&session, "detach-message")
            .await
            .unwrap()
            .unwrap()
            .agent,
        old_owner
    );
    assert_eq!(
        store.load_compaction(&session, &old_owner).await.unwrap(),
        Some(("detached summary".into(), 0))
    );
    assert!(
        store
            .load_compaction(&session, &new_owner)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn attach_admission_is_atomic_and_survives_store_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("attach.db");
    let client = ClientId::new("client-a");
    let other_client = ClientId::new("client-b");
    let key = "attach-key";
    let first_session = SessionId::new("ses_attach_first");
    let second_session = SessionId::new("ses_attach_second");

    {
        let store = Store::open(&db_path).unwrap();
        let admitted = store
            .admit_attach(&client, key, "new:/tmp:model:build", &first_session, true)
            .await
            .unwrap();
        let admitted = match admitted {
            AttachAdmissionResult::Admitted(admission) => admission,
            other => panic!("expected admitted attach, got {other:?}"),
        };
        assert_eq!(admitted.session_id, first_session);
        assert_eq!(admitted.fingerprint, "new:/tmp:model:build");
        assert!(admitted.allow_create);

        assert_eq!(
            store
                .admit_attach(&client, key, "new:/tmp:model:build", &second_session, false)
                .await
                .unwrap(),
            AttachAdmissionResult::Duplicate(admitted.clone())
        );
        assert_eq!(
            store
                .admit_attach(
                    &client,
                    key,
                    "new:/other:model:build",
                    &second_session,
                    false
                )
                .await
                .unwrap(),
            AttachAdmissionResult::Conflict(admitted.clone())
        );
        assert!(matches!(
            store
                .admit_attach(&other_client, key, "other", &second_session, false)
                .await
                .unwrap(),
            AttachAdmissionResult::Admitted(_)
        ));
        store.close().await;
    }

    let store = Store::open(&db_path).unwrap();
    let persisted = store
        .get_attach_admission(&client, key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.session_id, first_session);
    assert_eq!(persisted.fingerprint, "new:/tmp:model:build");
    assert!(persisted.allow_create);
    assert_eq!(
        store
            .admit_attach(&client, key, "new:/tmp:model:build", &second_session, false)
            .await
            .unwrap(),
        AttachAdmissionResult::Duplicate(persisted.clone())
    );
    assert_eq!(
        store
            .admit_attach(
                &client,
                key,
                "new:/other:model:build",
                &second_session,
                false
            )
            .await
            .unwrap(),
        AttachAdmissionResult::Conflict(persisted)
    );
    store.close().await;
}

#[test]
fn scoped_admission_loads_cover_only_their_session_and_agent() {
    let mut conn = store_conn();
    let first = SessionId::new("ses_scope_a");
    let second = SessionId::new("ses_scope_b");
    queries::upsert_session(&mut conn, &sample_meta(first.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_session(&mut conn, &sample_meta(second.as_str(), "/tmp", 1)).unwrap();
    let main = AgentId::main();
    let sub = AgentId::generate();

    for (session, prefix) in [(&first, "a"), (&second, "b")] {
        for seq in 0..3u64 {
            queries::append_user_message(
                &mut conn,
                session,
                &main,
                seq,
                &format!("{prefix}-{seq}"),
                100 + seq,
                &Message::user("scoped"),
            )
            .unwrap();
        }
    }
    queries::append_user_message(
        &mut conn,
        &first,
        &sub,
        0,
        "a-sub",
        7,
        &Message::user("sub"),
    )
    .unwrap();
    queries::mark_message_admission_terminal(&conn, &first, "a-1", Some("completed")).unwrap();
    queries::mark_message_admission_running(&conn, &first, "a-2", "trn_running").unwrap();

    let scoped = queries::load_message_admissions_for_session(&conn, &first).unwrap();
    assert_eq!(scoped.len(), 4);
    let keys: Vec<(Option<u64>, &str)> = scoped
        .iter()
        .map(|admission| (admission.message_seq, admission.message_id.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            (Some(0), "a-0"),
            (Some(0), "a-sub"),
            (Some(1), "a-1"),
            (Some(2), "a-2"),
        ]
    );
    assert!(scoped.iter().all(|admission| admission.session == first));

    let unresolved = queries::load_unresolved_message_admissions(&conn, &first, &main).unwrap();
    let unresolved_keys: Vec<&str> = unresolved
        .iter()
        .map(|admission| admission.message_id.as_str())
        .collect();
    assert_eq!(unresolved_keys, vec!["a-0", "a-2"]);
    assert!(
        unresolved
            .iter()
            .all(|admission| admission.state != MessageAdmissionState::Terminal)
    );

    let sub_unresolved = queries::load_unresolved_message_admissions(&conn, &first, &sub).unwrap();
    assert_eq!(sub_unresolved.len(), 1);
    assert_eq!(sub_unresolved[0].message_id, "a-sub");
    assert!(
        queries::load_unresolved_message_admissions(&conn, &second, &sub)
            .unwrap()
            .is_empty()
    );
    assert_eq!(queries::load_message_admissions(&conn).unwrap().len(), 7);
}

#[test]
fn paged_admission_load_walks_every_key_exactly_once() {
    let mut conn = store_conn();
    let sessions = [
        SessionId::new("ses_page_a"),
        SessionId::new("ses_page_b"),
        SessionId::new("ses_page_c"),
    ];
    for session in &sessions {
        queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    }
    for seq in 0..4u64 {
        for session in &sessions {
            queries::append_user_message(
                &mut conn,
                session,
                &AgentId::main(),
                seq,
                &format!("{seq}"),
                seq,
                &Message::user("paged"),
            )
            .unwrap();
        }
    }

    let expected = queries::load_message_admissions(&conn).unwrap();
    let mut walked: Vec<(String, Option<u64>, String)> = Vec::new();
    let mut cursor: Option<AdmissionCursor> = None;
    let mut pages = 0;
    loop {
        let page = queries::load_message_admissions_page(&conn, 5, cursor.as_ref()).unwrap();
        assert!(page.admissions.len() <= 5);
        for admission in &page.admissions {
            walked.push((
                admission.session.as_str().to_string(),
                admission.message_seq,
                admission.message_id.clone(),
            ));
        }
        pages += 1;
        assert!(pages <= 16, "keyset walk did not converge");
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let expected_keys: Vec<(String, Option<u64>, String)> = expected
        .iter()
        .map(|admission| {
            (
                admission.session.as_str().to_string(),
                admission.message_seq,
                admission.message_id.clone(),
            )
        })
        .collect();
    assert_eq!(walked, expected_keys);

    let last = queries::load_message_admissions_page(&conn, 100, None).unwrap();
    assert!(last.next_cursor.is_none());
    assert_eq!(last.admissions.len(), expected.len());
}

#[test]
fn message_window_after_compaction_is_bounded() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_window");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    for seq in 0..10u64 {
        queries::append_message(
            &mut conn,
            &session,
            &agent,
            seq,
            &Message::user(format!("row {seq}")),
        )
        .unwrap();
    }
    queries::save_compaction(&mut conn, &session, &agent, "summary", 4).unwrap();

    assert_eq!(
        queries::max_message_seq(&conn, &session, &agent).unwrap(),
        Some(9)
    );

    let head = queries::load_messages_after(&conn, &session, &agent, None, 3).unwrap();
    assert_eq!(
        head.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(head.has_more);

    let tail = queries::load_messages_after(&conn, &session, &agent, Some(4), 3).unwrap();
    assert_eq!(
        tail.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
    assert!(tail.has_more);

    let rest = queries::load_messages_after(&conn, &session, &agent, Some(4), 100).unwrap();
    assert_eq!(
        rest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![5, 6, 7, 8, 9]
    );
    assert!(!rest.has_more);

    let empty = queries::load_messages_after(&conn, &session, &agent, Some(9), 10).unwrap();
    assert!(empty.rows.is_empty());
    assert!(!empty.has_more);

    let other = AgentId::generate();
    assert_eq!(
        queries::max_message_seq(&conn, &session, &other).unwrap(),
        None
    );
    assert!(
        queries::load_messages_after(&conn, &session, &other, Some(4), 10)
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn session_pagination_is_deterministic_across_equal_timestamps() {
    let mut conn = store_conn();
    let ids = [
        "ses_pag_1",
        "ses_pag_2",
        "ses_pag_3",
        "ses_pag_4",
        "ses_pag_5",
    ];
    for (index, id) in ids.iter().enumerate() {
        let cwd = if index < 3 { "/repo" } else { "/other" };
        queries::upsert_session(&mut conn, &sample_meta(id, cwd, 500)).unwrap();
    }
    let newest = sample_meta("ses_pag_new", "/repo", 900);
    queries::upsert_session(&mut conn, &newest).unwrap();

    let filter = SessionFilter {
        cwd: None,
        limit: 2,
    };
    let mut walked: Vec<SessionId> = Vec::new();
    let mut cursor: Option<SessionCursor> = None;
    loop {
        let page = queries::list_sessions_page(&conn, &filter, cursor.as_ref()).unwrap();
        for meta in &page.sessions {
            walked.push(meta.id.clone());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked.len(), 6);
    let unique: std::collections::HashSet<&SessionId> = walked.iter().collect();
    assert_eq!(unique.len(), 6);
    assert_eq!(walked[0], newest.id);
    let tail: Vec<&str> = walked[1..].iter().map(|id| id.as_str()).collect();
    assert_eq!(
        tail,
        vec![
            "ses_pag_1",
            "ses_pag_2",
            "ses_pag_3",
            "ses_pag_4",
            "ses_pag_5"
        ]
    );

    let repeat = queries::list_sessions_page(&conn, &filter, None).unwrap();
    assert_eq!(
        repeat
            .sessions
            .iter()
            .map(|meta| meta.id.clone())
            .collect::<Vec<_>>(),
        walked[..2].to_vec()
    );

    let cwd_filter = SessionFilter {
        cwd: Some(PathBuf::from("/repo")),
        limit: 2,
    };
    let first_page = queries::list_sessions_page(&conn, &cwd_filter, None).unwrap();
    let first_ids: Vec<SessionId> = first_page
        .sessions
        .iter()
        .map(|meta| meta.id.clone())
        .collect();
    assert_eq!(first_ids, vec![newest.id, SessionId::new("ses_pag_1")]);
    let second_page =
        queries::list_sessions_page(&conn, &cwd_filter, first_page.next_cursor.as_ref()).unwrap();
    assert_eq!(
        second_page
            .sessions
            .iter()
            .map(|meta| meta.id.clone())
            .collect::<Vec<_>>(),
        vec![SessionId::new("ses_pag_2"), SessionId::new("ses_pag_3")]
    );
    assert!(second_page.next_cursor.is_none());
}

#[test]
fn task_admission_is_durable_and_reserves_one_task_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("task-admission.db");
    let session = SessionId::new("ses_task_admission");
    let owner = AgentId::main();
    let key = "call-42";
    let fingerprint = "cargo test";

    let reserved = TaskId::generate();
    {
        let mut conn = Connection::open(&db_path).unwrap();
        pacode_store::schema::migrate(&mut conn).unwrap();
        queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();

        let admitted =
            queries::admit_task(&mut conn, &session, key, fingerprint, &reserved, &owner).unwrap();
        let admitted = match admitted {
            TaskAdmissionResult::Admitted(admission) => admission,
            other => panic!("expected admitted task, got {other:?}"),
        };
        assert_eq!(admitted.task_id, reserved);
        assert_eq!(admitted.owner, owner);
        assert_eq!(admitted.fingerprint, fingerprint);

        let replay = queries::admit_task(
            &mut conn,
            &session,
            key,
            fingerprint,
            &TaskId::generate(),
            &owner,
        )
        .unwrap();
        match replay {
            TaskAdmissionResult::Duplicate(admission) => assert_eq!(admission.task_id, reserved),
            other => panic!("expected duplicate task admission, got {other:?}"),
        }

        let conflict = queries::admit_task(
            &mut conn,
            &session,
            key,
            "cargo build",
            &TaskId::generate(),
            &owner,
        )
        .unwrap();
        match conflict {
            TaskAdmissionResult::Conflict(admission) => assert_eq!(admission.task_id, reserved),
            other => panic!("expected conflicting task admission, got {other:?}"),
        }
    }

    let conn = Connection::open(&db_path).unwrap();
    let persisted = queries::get_task_admission(&conn, &session, key)
        .unwrap()
        .expect("task admission must survive a restart");
    assert_eq!(persisted.task_id, reserved);
    assert_eq!(
        queries::get_task_admission_by_task(&conn, &reserved)
            .unwrap()
            .map(|admission| admission.idempotency_key),
        Some(key.to_string())
    );
    assert!(
        queries::get_task_admission_by_task(&conn, &TaskId::generate())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        persisted.created_at_ms,
        admission_created_at(&conn, &session, key)
    );
}

fn admission_created_at(conn: &Connection, session: &SessionId, key: &str) -> u64 {
    conn.query_row(
        "SELECT created_at FROM task_admissions WHERE session_id = ?1 AND idempotency_key = ?2;",
        rusqlite::params![session.as_str(), key],
        |row| row.get::<_, i64>(0),
    )
    .unwrap() as u64
}

#[test]
fn task_admission_is_scoped_to_session() {
    let mut conn = store_conn();
    let first = SessionId::new("ses_task_first");
    let second = SessionId::new("ses_task_second");
    queries::upsert_session(&mut conn, &sample_meta(first.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_session(&mut conn, &sample_meta(second.as_str(), "/tmp", 1)).unwrap();
    let owner = AgentId::main();

    let first_task = TaskId::generate();
    let second_task = TaskId::generate();
    assert!(matches!(
        queries::admit_task(&mut conn, &first, "same-key", "cmd", &first_task, &owner).unwrap(),
        TaskAdmissionResult::Admitted(_)
    ));
    assert!(matches!(
        queries::admit_task(&mut conn, &second, "same-key", "cmd", &second_task, &owner).unwrap(),
        TaskAdmissionResult::Admitted(_)
    ));
    assert_eq!(
        queries::get_task_admission(&conn, &first, "same-key")
            .unwrap()
            .map(|admission| admission.task_id),
        Some(first_task)
    );
    assert_eq!(
        queries::get_task_admission(&conn, &second, "same-key")
            .unwrap()
            .map(|admission| admission.task_id),
        Some(second_task)
    );
}

#[test]
fn legacy_usage_rows_are_idempotent_per_agent_turn() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_usage_legacy");
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    let main = AgentId::main();
    let sub = AgentId::generate();
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 10,
        ..Usage::default()
    };

    assert!(
        queries::add_usage_for_turn(&mut conn, &session, &main, 1, None, &usage, Some(0.01))
            .unwrap()
    );
    assert!(
        !queries::add_usage_for_turn(&mut conn, &session, &main, 1, None, &usage, Some(0.01))
            .unwrap()
    );

    let totals = queries::usage_totals(&conn, &session).unwrap();
    assert_eq!(totals.input, 100);
    assert_eq!(totals.output, 10);
    assert_eq!(totals.turns, 1);
    assert!((totals.cost_usd.unwrap() - 0.01).abs() < 1e-9);

    assert!(queries::add_usage_for_turn(&mut conn, &session, &sub, 1, None, &usage, None).unwrap());
    let totals = queries::usage_totals(&conn, &session).unwrap();
    assert_eq!(totals.input, 200);
    assert_eq!(totals.turns, 2);

    assert!(
        queries::add_usage_for_turn(&mut conn, &session, &main, 2, Some("trn_2"), &usage, None)
            .unwrap()
    );
    assert!(
        !queries::add_usage_for_turn(&mut conn, &session, &main, 2, Some("trn_2"), &usage, None)
            .unwrap()
    );
    assert!(
        queries::add_usage_for_turn(&mut conn, &session, &main, 2, Some("trn_3"), &usage, None)
            .unwrap()
    );
    let totals = queries::usage_totals(&conn, &session).unwrap();
    assert_eq!(totals.input, 400);
    assert_eq!(totals.turns, 4);
}

#[test]
fn detach_moves_only_messages_past_the_retained_boundary() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_detach_set");
    let old_owner = AgentId::main();
    let new_owner = AgentId::generate();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_agent(
        &mut conn,
        &session,
        &agent_info(old_owner.clone(), "main"),
        None,
    )
    .unwrap();
    queries::upsert_agent(
        &mut conn,
        &session,
        &agent_info(new_owner.clone(), "detached"),
        None,
    )
    .unwrap();

    for seq in 0..6u64 {
        queries::append_user_message(
            &mut conn,
            &session,
            &old_owner,
            seq,
            &format!("msg-{seq}"),
            seq,
            &Message::user(format!("token{seq}")),
        )
        .unwrap();
    }
    queries::save_compaction(&mut conn, &session, &old_owner, "summary", 3).unwrap();
    let handover: Vec<(u64, Arc<Message>)> = vec![
        (4, Arc::new(Message::user("token4"))),
        (5, Arc::new(Message::user("token5"))),
    ];

    queries::detach_agent(
        &mut conn,
        &session,
        &agent_info(new_owner.clone(), "detached"),
        None,
        &handover,
        &[0, 1],
        Some(3),
        &old_owner,
        &new_owner,
    )
    .unwrap();

    let kept = queries::load_messages(&conn, &session, &old_owner).unwrap();
    assert_eq!(
        kept.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let moved = queries::load_messages(&conn, &session, &new_owner).unwrap();
    assert_eq!(
        moved.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![4, 5]
    );

    for seq in 0..4u64 {
        assert_eq!(
            queries::get_message_admission(&conn, &session, &format!("msg-{seq}"))
                .unwrap()
                .unwrap()
                .agent,
            old_owner,
            "admission msg-{seq} must stay with the old owner"
        );
    }
    for seq in 4..6u64 {
        assert_eq!(
            queries::get_message_admission(&conn, &session, &format!("msg-{seq}"))
                .unwrap()
                .unwrap()
                .agent,
            new_owner,
            "admission msg-{seq} must follow the moved message"
        );
    }
    assert_eq!(
        queries::load_compaction(&conn, &session, &new_owner).unwrap(),
        Some(("summary".to_string(), 3))
    );

    let hits = queries::search(&conn, "token5", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].agent, new_owner);
    assert_eq!(hits[0].seq, 5);
}

#[test]
fn detach_without_retained_seqs_moves_the_whole_tail() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_detach_all");
    let old_owner = AgentId::main();
    let new_owner = AgentId::generate();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_agent(
        &mut conn,
        &session,
        &agent_info(old_owner.clone(), "main"),
        None,
    )
    .unwrap();

    for seq in 0..3u64 {
        queries::append_message(
            &mut conn,
            &session,
            &old_owner,
            seq,
            &Message::user(format!("row {seq}")),
        )
        .unwrap();
    }
    let handover: Vec<(u64, Arc<Message>)> = (0..3u64)
        .map(|seq| (seq, Arc::new(Message::user(format!("row {seq}")))))
        .collect();

    queries::detach_agent(
        &mut conn,
        &session,
        &agent_info(new_owner.clone(), "detached"),
        None,
        &handover,
        &[],
        None,
        &old_owner,
        &new_owner,
    )
    .unwrap();

    assert!(
        queries::load_messages(&conn, &session, &old_owner)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        queries::load_messages(&conn, &session, &new_owner)
            .unwrap()
            .len(),
        3
    );
    let hits = queries::search(&conn, "row", 10).unwrap();
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|hit| hit.agent == new_owner));
}

fn indexed_row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM messages_fts_docsize;", [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn message_row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM messages;", [], |row| row.get(0))
        .unwrap()
}
fn query_plan(conn: &Connection, sql: &str, binds: usize) -> Vec<String> {
    let binds: Vec<Option<i64>> = vec![None; binds];
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    stmt.query_map(rusqlite::params_from_iter(binds.iter()), |row| {
        row.get::<_, String>(3)
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

#[test]
fn keyset_windows_are_served_by_their_index_without_a_sort() {
    let conn = store_conn();
    let cases = [
        (
            "admission page",
            4,
            r#"
            SELECT session_id, message_id FROM message_admissions
            WHERE ?1 IS NULL
               OR session_id > ?1
               OR (session_id = ?1 AND (
                      COALESCE(message_seq, -1) > ?2
                      OR (COALESCE(message_seq, -1) = ?2 AND message_id > ?3)))
            ORDER BY session_id ASC, message_seq ASC, message_id ASC
            LIMIT ?4;
            "#,
            "idx_message_admissions_page",
        ),
        (
            "unresolved admissions",
            2,
            r#"
            SELECT session_id, message_id FROM message_admissions
            WHERE session_id = ?1 AND agent_id = ?2 AND state <> 'terminal'
            ORDER BY message_seq, message_id;
            "#,
            "idx_message_admissions_unresolved",
        ),
        (
            "session page",
            4,
            r#"
            SELECT id FROM sessions
            WHERE cwd = ?1 AND (updated_at < ?2 OR (updated_at = ?2 AND id > ?3))
            ORDER BY updated_at DESC, id ASC
            LIMIT ?4;
            "#,
            "idx_sessions_cwd_updated",
        ),
        (
            "session page without a filter",
            1,
            r#"
            SELECT id FROM sessions
            WHERE 1 = 1
            ORDER BY updated_at DESC, id ASC
            LIMIT ?1;
            "#,
            "idx_sessions_updated_id",
        ),
        (
            "message window",
            4,
            r#"
            SELECT seq, content_json FROM messages
            WHERE session_id = ?1 AND agent_id = ?2 AND seq > ?3
              AND NOT EXISTS (SELECT 1 FROM message_admissions AS admission
                              WHERE admission.session_id = ?1
                                AND admission.agent_id = ?2
                                AND admission.message_seq = messages.seq
                                AND admission.state = 'terminal'
                                AND admission.terminal_stop IN ('interrupted', 'cancelled', 'skipped'))
            ORDER BY seq ASC
            LIMIT ?4;
            "#,
            "idx_messages_lookup",
        ),
        (
            "search",
            2,
            r#"
            SELECT m.session_id, m.agent_id, m.seq, snippet(messages_fts, 0, '[', ']', '…', 12), m.text
            FROM messages_fts
            JOIN messages m ON m.id = messages_fts.rowid
            WHERE messages_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2;
            "#,
            "messages_fts VIRTUAL TABLE INDEX",
        ),
    ];

    for (name, binds, sql, index) in cases {
        let plan = query_plan(&conn, sql, binds);
        assert!(
            plan.iter().any(|step| step.contains(index)),
            "{name} does not use {index}: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{name} still sorts its window: {plan:?}"
        );
    }
}

#[test]
fn message_window_refills_past_cancelled_admissions() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_refill");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    for seq in 0..10u64 {
        queries::append_user_message(
            &mut conn,
            &session,
            &agent,
            seq,
            &format!("row-{seq}"),
            seq,
            &Message::user(format!("row {seq}")),
        )
        .unwrap();
    }
    for seq in [1u64, 3, 5, 7, 9] {
        assert!(
            queries::mark_message_admission_terminal(
                &conn,
                &session,
                &format!("row-{seq}"),
                Some("interrupted"),
            )
            .unwrap()
        );
    }

    let head = queries::load_messages_after(&conn, &session, &agent, None, 4).unwrap();
    assert_eq!(
        head.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![0, 2, 4, 6]
    );
    assert!(head.has_more);

    let tail = queries::load_messages_after(&conn, &session, &agent, Some(6), 10).unwrap();
    assert_eq!(
        tail.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![8]
    );
    assert!(!tail.has_more);

    let whole = queries::load_messages_after(&conn, &session, &agent, None, 100).unwrap();
    assert_eq!(
        whole.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![0, 2, 4, 6, 8]
    );
    assert!(!whole.has_more);

    let cancelled_still_readable =
        queries::load_messages_before(&conn, &session, &agent, None, 4).unwrap();
    assert_eq!(
        cancelled_still_readable
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        vec![6, 7, 8, 9],
        "the transcript window keeps showing interrupted turns"
    );
}

#[test]
fn detach_purges_a_multi_chunk_tail_without_leaving_fts_rows_behind() {
    const ROWS: u64 = 700;
    const SUMMARY_UPTO: u64 = 20;

    let mut conn = store_conn();
    let session = SessionId::new("ses_detach_chunks");
    let old_owner = AgentId::main();
    let new_owner = AgentId::generate();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_agent(
        &mut conn,
        &session,
        &agent_info(old_owner.clone(), "main"),
        None,
    )
    .unwrap();

    let stored: Vec<Arc<Message>> = (0..ROWS)
        .map(|seq| Arc::new(Message::user(format!("tail{seq}"))))
        .collect();
    for (seq, message) in stored.iter().enumerate() {
        queries::append_message(
            &mut conn,
            &session,
            &old_owner,
            seq as u64,
            message.as_ref(),
        )
        .unwrap();
    }
    assert_eq!(indexed_row_count(&conn), message_row_count(&conn));

    let handover: Vec<(u64, Arc<Message>)> = stored
        .iter()
        .enumerate()
        .map(|(seq, message)| (seq as u64, message.clone()))
        .collect();
    let retained: Vec<u64> = (0..=10).collect();

    queries::detach_agent(
        &mut conn,
        &session,
        &agent_info(new_owner.clone(), "detached"),
        None,
        &handover,
        &retained,
        Some(SUMMARY_UPTO),
        &old_owner,
        &new_owner,
    )
    .unwrap();

    assert_eq!(
        queries::load_messages(&conn, &session, &old_owner)
            .unwrap()
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        (0..=SUMMARY_UPTO).collect::<Vec<_>>()
    );
    assert_eq!(
        queries::load_messages(&conn, &session, &new_owner)
            .unwrap()
            .len(),
        ROWS as usize
    );
    assert_eq!(
        indexed_row_count(&conn),
        message_row_count(&conn),
        "every purged row left its search entry behind"
    );

    let hits = queries::search(&conn, "tail500", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].agent, new_owner);
    assert_eq!(hits[0].seq, 500);
    let moved = queries::search(&conn, "tail69", 10).unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].agent, new_owner);
    assert_eq!(moved[0].seq, 69);
    let shared_prefix = queries::search(&conn, "tail5", 10).unwrap();
    let mut owners: Vec<(u64, AgentId)> = shared_prefix
        .into_iter()
        .map(|hit| (hit.seq, hit.agent))
        .collect::<Vec<_>>();
    owners.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    assert_eq!(
        owners.len(),
        2,
        "a retained row is searchable once per owner, a moved row only under the new one"
    );
    assert!(owners.contains(&(5, old_owner.clone())));
    assert!(owners.contains(&(5, new_owner.clone())));

    queries::rollback_detach(&mut conn, &session, &new_owner, &handover, &old_owner).unwrap();
    assert_eq!(
        queries::load_messages(&conn, &session, &old_owner)
            .unwrap()
            .len(),
        ROWS as usize
    );
    assert!(
        queries::load_messages(&conn, &session, &new_owner)
            .unwrap()
            .is_empty()
    );
    assert_eq!(indexed_row_count(&conn), message_row_count(&conn));
    assert_eq!(queries::search(&conn, "tail500", 10).unwrap().len(), 1);
}

#[test]
fn delete_session_purges_every_message_it_owns() {
    const ROWS: u64 = 600;

    let mut conn = store_conn();
    let deleted = SessionId::new("ses_delete_bulk");
    let kept = SessionId::new("ses_delete_kept");
    queries::upsert_session(&mut conn, &sample_meta(deleted.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_session(&mut conn, &sample_meta(kept.as_str(), "/tmp", 1)).unwrap();
    for seq in 0..ROWS {
        queries::append_message(
            &mut conn,
            &deleted,
            &AgentId::main(),
            seq,
            &Message::user(format!("bulk{seq}")),
        )
        .unwrap();
    }
    queries::append_message(
        &mut conn,
        &kept,
        &AgentId::main(),
        0,
        &Message::user("keeperline"),
    )
    .unwrap();
    assert_eq!(indexed_row_count(&conn), ROWS as i64 + 1);

    queries::delete_session(&mut conn, &deleted).unwrap();

    assert_eq!(message_row_count(&conn), 1);
    assert_eq!(indexed_row_count(&conn), 1);
    assert!(queries::search(&conn, "bulk", 10).unwrap().is_empty());
    assert_eq!(queries::search(&conn, "keeperline", 10).unwrap().len(), 1);
}

#[test]
fn a_session_deleted_behind_the_store_still_takes_its_index_entries() {
    let mut conn = store_conn();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let session = SessionId::new("ses_cascade_index");
    let kept = SessionId::new("ses_cascade_kept");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    queries::upsert_session(&mut conn, &sample_meta(kept.as_str(), "/tmp", 1)).unwrap();
    for seq in 0..3u64 {
        queries::append_message(
            &mut conn,
            &session,
            &agent,
            seq,
            &Message::user(format!("cascade{seq} shared")),
        )
        .unwrap();
    }
    queries::append_message(
        &mut conn,
        &kept,
        &agent,
        0,
        &Message::user("cascade kept shared"),
    )
    .unwrap();
    assert_eq!(indexed_row_count(&conn), 4);

    conn.execute("DELETE FROM sessions WHERE id = ?1;", [session.as_str()])
        .unwrap();

    assert_eq!(message_row_count(&conn), 1);
    assert_eq!(
        indexed_row_count(&conn),
        1,
        "a delete that bypasses the store left index entries behind"
    );
    let hits = queries::search(&conn, "shared", 10).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "a purged row took the terms of the rows that stayed"
    );
    assert_eq!(hits[0].session, kept);
    assert_eq!(hits[0].seq, 0);
}

#[test]
fn a_message_without_searchable_text_stays_out_of_the_index() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_tool_only");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    let tool_call = Message::new(
        pacode_types::Role::Assistant,
        vec![pacode_types::ContentBlock::ToolUse {
            id: CallId::new("call_1"),
            name: "read".to_string(),
            input: serde_json::Value::Null,
        }],
    );
    queries::append_message(&mut conn, &session, &agent, 0, &tool_call).unwrap();
    queries::append_message(
        &mut conn,
        &session,
        &agent,
        1,
        &Message::user("сопроводительное сообщение"),
    )
    .unwrap();

    assert_eq!(message_row_count(&conn), 2);
    assert_eq!(
        indexed_row_count(&conn),
        1,
        "a message with nothing to search for must not take an index slot"
    );
    assert_eq!(
        queries::search(&conn, "сопроводительное", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn re_appending_the_same_message_keeps_one_index_entry() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_reappend");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    let message = Message::user("повторная запись того же сообщения");
    queries::append_message(&mut conn, &session, &agent, 0, &message).unwrap();
    queries::append_message(&mut conn, &session, &agent, 0, &message).unwrap();

    assert_eq!(message_row_count(&conn), 1);
    assert_eq!(indexed_row_count(&conn), 1);
    let hits = queries::search(&conn, "повторная", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, 0);
}

#[test]
fn the_search_index_can_be_rebuilt_from_the_messages_table() {
    let mut conn = store_conn();
    let session = SessionId::new("ses_rebuild_index");
    let agent = AgentId::main();
    queries::upsert_session(&mut conn, &sample_meta(session.as_str(), "/tmp", 1)).unwrap();
    for seq in 0..3u64 {
        queries::append_message(
            &mut conn,
            &session,
            &agent,
            seq,
            &Message::user(format!("восстанавливаемый{seq}")),
        )
        .unwrap();
    }

    conn.execute_batch("INSERT INTO messages_fts (messages_fts) VALUES ('delete-all');")
        .unwrap();
    assert_eq!(indexed_row_count(&conn), 0);
    assert!(
        queries::search(&conn, "восстанавливаемый", 10)
            .unwrap()
            .is_empty()
    );

    conn.execute_batch(
        "INSERT INTO messages_fts (rowid, text) SELECT id, text FROM messages WHERE text <> '';",
    )
    .unwrap();

    assert_eq!(indexed_row_count(&conn), message_row_count(&conn));
    let hits = queries::search(&conn, "восстанавливаемый2", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, 2);
}

#[tokio::test]
async fn a_v8_file_database_upgrades_to_a_content_backed_index_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v8-upgrade.db");
    let session = SessionId::new("ses_v8_upgrade");
    let agent = AgentId::main();
    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_session(&sample_meta(session.as_str(), "/tmp", 1_000))
            .await
            .unwrap();
        store
            .append_message(&session, &agent, 0, &Message::user("наследие v8 в индексе"))
            .await
            .unwrap();
        store.close().await;
    }

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            DROP TRIGGER messages_fts_ai;
            DROP TRIGGER messages_fts_au;
            DROP TRIGGER messages_fts_ad;
            DROP TABLE messages_fts;
            ALTER TABLE messages DROP COLUMN text;
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                text,
                content='',
                tokenize='unicode61'
            );
            PRAGMA user_version = 8;
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages_fts (rowid, text) SELECT id, 'наследие v8 в индексе' FROM messages;",
            [],
        )
        .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let hits = store.search("наследие", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "the upgrade left the old index unsearchable");
    assert_eq!(hits[0].session, session);
    assert_eq!(hits[0].seq, 0);
    store.close().await;

    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.search("наследие", 10).await.unwrap().len(),
        1,
        "the upgraded index did not survive the restart"
    );
    reopened.close().await;
}
