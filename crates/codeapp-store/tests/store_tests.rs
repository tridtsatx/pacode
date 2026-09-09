//! Tests for `codeapp-store`.

#![deny(warnings, clippy::all)]

use std::path::PathBuf;

use codeapp_store::{SessionFilter, Store};
use codeapp_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, CallId, Effort, Message, Mode, ModelRoute, Plan,
    PlanItem, PlanStatus, ProgressSource, SessionId, SessionMeta, TaskId, TaskInfo, TaskProgress,
    TaskStatus, Usage,
};

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
