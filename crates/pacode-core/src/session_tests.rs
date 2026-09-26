use std::sync::Arc;
use std::time::Duration;

use crate::session::Session;
use pacode_config::Paths;
use pacode_exec::{TaskManager, TaskSpec};
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::{Tool, ToolCtx, ToolError, ToolHost, ToolKind, ToolOutput, ToolRegistry};
use pacode_types::{
    AgentId, AgentStatus, Attach, CallId, Config, ContentBlock, Effort, Event, Message, ModelRoute,
    Reply, Request, Role, TranscriptKind, TurnId, TurnStop,
};

use crate::core::{Core, CoreDeps};

/// A tool that panics inside `call` — exercises the turn-task supervisor: a
/// panic in `run_turn` must still clear the cancel token, land a terminal
/// status and emit `TurnEnded` instead of leaving the agent "running" forever.
struct PanicTool;
#[async_trait::async_trait]
impl Tool for PanicTool {
    fn name(&self) -> &'static str {
        "panic_tool"
    }
    fn description(&self) -> &'static str {
        "Panics on call (test fixture)"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        panic!("intentional test panic in panic_tool");
    }
}

/// A tool that only resolves once the turn's cancel token fires — lets a test
/// land an interrupt while dispatch is mid-execution.
struct BlockTool;
#[async_trait::async_trait]
impl Tool for BlockTool {
    fn name(&self) -> &'static str {
        "block_tool"
    }
    fn description(&self) -> &'static str {
        "Blocks until the turn is cancelled (test fixture)"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        ctx.cancel.cancelled().await;
        Err(ToolError::Cancelled)
    }
}

async fn build_test_core(
    store: Store,
    temp_dir: &tempfile::TempDir,
    mock_provider: Arc<MockProvider>,
) -> Arc<Core> {
    let exec_cfg = pacode_types::ExecConfig {
        yield_after_secs: 1,
        stall_secs: 100,
        max_spool_bytes: 50 * 1024 * 1024,
        tail_bytes: 64 * 1024,
        kill_grace_secs: 5,
        max_tasks: 64,
        default_timeout_secs: 60,
    };
    let tasks = TaskManager::new(temp_dir.path().to_path_buf(), exec_cfg);
    let mcp = McpPool::new(Default::default(), None, None);

    let mut reg = ProviderRegistry::empty();
    reg.insert(mock_provider);
    reg.set_default_route(Some(ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    }));

    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());

    let paths = Paths::under(temp_dir.path());
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(PanicTool));
    tools.register(Arc::new(BlockTool));

    let deps = CoreDeps {
        marketplace: std::sync::Arc::new(pacode_plugin::marketplace::Marketplace::new(
            std::sync::Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
            paths.cache_dir.join("marketplace"),
            paths.cache_dir.join("plugins"),
            pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
        )),
        config: Arc::new(config),
        paths,
        providers: Arc::new(reg),
        tools,
        tasks,
        mcp,
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };

    Core::new(deps).await
}

async fn setup_test_session() -> (
    Arc<Core>,
    Arc<MockProvider>,
    tempfile::TempDir,
    pacode_types::SessionId,
) {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let mock_provider = Arc::new(MockProvider::new("mock"));
    let core = build_test_core(store, &temp_dir, mock_provider.clone()).await;
    let session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();

    if let Some(s) = core.session(&session_id)
        && let Ok(mut m) = s.meta.write()
    {
        m.name = Some("test session".to_string());
    }

    (core, mock_provider, temp_dir, session_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_identity_accessor_and_swap() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();

    assert_eq!(main.id(), AgentId::main());
    assert_eq!(*main.id_arc(), AgentId::main());
    assert!(main.with_id(|id| id.is_main()));

    let new_id = AgentId::new("agt_test_swap_123");
    main.set_id(new_id.clone());

    assert_eq!(main.id(), new_id);
    assert_eq!(*main.id_arc(), new_id);
    assert!(main.with_id(|id| !id.is_main() && id.as_str() == "agt_test_swap_123"));
}

#[tokio::test(flavor = "multi_thread")]
async fn close_and_resume_share_one_session_lifecycle_owner() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let original_main = session.main_agent().unwrap();
    let held_turn = original_main.turn_lock.clone().lock_owned().await;

    let close = tokio::spawn({
        let core = core.clone();
        let session_id = session_id.clone();
        async move { core.close_session(&session_id).await }
    });
    wait_until(|| original_main.stop_requested()).await;
    let resume = tokio::spawn({
        let core = core.clone();
        let session_id = session_id.clone();
        async move {
            core.open_session(Attach::Resume {
                session: session_id,
            })
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !resume.is_finished(),
        "resume must wait for close quiescence"
    );

    drop(held_turn);
    close.await.unwrap();
    assert_eq!(resume.await.unwrap().unwrap(), session_id);
    let resumed = core.session(&session_id).unwrap();
    assert!(!Arc::ptr_eq(&resumed, &session));
    assert!(!Arc::ptr_eq(&resumed.main_agent().unwrap(), &original_main));
    assert!(!original_main.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn close_fences_spawn_and_continue_agent_operations() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let close = tokio::spawn({
        let core = core.clone();
        let session_id = session_id.clone();
        async move { core.close_session(&session_id).await }
    });
    wait_until(|| session.closed.load(std::sync::atomic::Ordering::SeqCst)).await;
    assert!(
        session
            .spawn_agent(pacode_tools::AgentSpec {
                name: Some("late".into()),
                prompt: "must not spawn".into(),
                tools: None,
                exclude_tools: None,
                fork: false,
                model: None,
                effort: None,
            })
            .await
            .is_err()
    );
    assert!(session.continue_agent(&AgentId::main()).await.is_err());
    close.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn latest_attach_reuses_existing_identity_without_recursive_lock() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let cwd = core.session(&session_id).unwrap().meta().cwd;
    let opened = tokio::time::timeout(
        Duration::from_secs(1),
        core.open_session_with_id(Attach::Latest { cwd }, session_id.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(opened, session_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_resolution_does_not_invert_alias_and_agent_locks() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let alias_id = AgentId::new("agt_alias_only");
    session.alias_agent(alias_id.clone(), main.clone());
    let resolver_session = session.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let agents = session.agents.write().unwrap();

    let (alias_was_available, resolved) = std::thread::scope(|scope| {
        let resolver = scope.spawn(move || {
            started_tx.send(()).unwrap();
            resolver_session.agent_for_host(&alias_id)
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let alias_was_available = session.agent_aliases.try_lock().is_ok();
        drop(agents);
        let resolved = resolver.join().unwrap();
        (alias_was_available, resolved)
    });

    assert!(alias_was_available);
    assert!(Arc::ptr_eq(&resolved.unwrap(), &main));
}

/// A turn owns its agent's turn lock for its whole lifetime, and a tool inside
/// that turn can need the session submit lock (spawning a subagent is a tool
/// call). So `InterruptAll` has to take the turn lock first: taking the submit
/// lock and then waiting for the turn left the turn waiting for a lock nobody
/// was going to release.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_all_waits_for_the_turn_without_holding_the_submit_lock() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let turn = main.turn_lock.clone().try_lock_owned().unwrap();

    let interrupt = tokio::spawn({
        let session = session.clone();
        async move { session.interrupt_all().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !interrupt.is_finished(),
        "InterruptAll must wait for the turn in flight"
    );
    assert!(
        session.submit_lock.try_lock().is_ok(),
        "InterruptAll must not hold the submit lock while waiting for the turn"
    );

    drop(turn);
    tokio::time::timeout(Duration::from_secs(2), interrupt)
        .await
        .expect("InterruptAll deadlocked waiting for the turn lock")
        .unwrap();
    assert!(!main.is_running());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_waits_for_the_turn_without_holding_the_submit_lock() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let turn = main.turn_lock.clone().try_lock_owned().unwrap();

    let close = tokio::spawn({
        let core = core.clone();
        let session_id = session_id.clone();
        async move { core.close_session(&session_id).await }
    });
    wait_until(|| session.closed.load(std::sync::atomic::Ordering::SeqCst)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !close.is_finished(),
        "close must wait for the turn in flight"
    );
    assert!(
        session.submit_lock.try_lock().is_ok(),
        "close must not hold the submit lock while waiting for the turn"
    );

    drop(turn);
    tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .expect("close deadlocked waiting for the turn lock")
        .unwrap();
    assert!(core.session_ids().is_empty());
    core.shutdown().await;
}

/// An admission whose claim failed goes back to the head of its agent's queue:
/// it was the one being replayed first, and a re-queue at the tail would let
/// later messages overtake it.
#[tokio::test(flavor = "multi_thread")]
async fn a_requeued_admission_goes_back_to_the_head_of_the_recovery_queue() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let agent = AgentId::main();

    session.queue_recovery_admissions(&agent, ["first".to_string(), "second".to_string()]);
    assert!(!session.queue_recovery_admission_first(&agent, "first".to_string()));
    assert_eq!(
        session.take_recovery_admissions(&agent),
        vec!["first".to_string(), "second".to_string()],
        "an already-queued admission keeps its place instead of duplicating"
    );

    session.queue_recovery_admissions(
        &agent,
        [
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ],
    );
    assert!(session.queue_recovery_admission_first(&agent, "retried".to_string()));
    assert_eq!(
        session.take_recovery_admissions(&agent),
        vec![
            "retried".to_string(),
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );

    let cap = crate::session::MAX_RECOVERY_ADMISSIONS;
    session.queue_recovery_admissions(&agent, (0..cap).map(|index| format!("cap-{index}")));
    assert!(session.queue_recovery_admission_first(&agent, "requeued".to_string()));
    let queued = session.take_recovery_admissions(&agent);
    assert_eq!(queued.len(), cap, "the queue stays bounded");
    assert_eq!(queued.first().map(String::as_str), Some("requeued"));
    assert_eq!(
        queued.last().map(String::as_str),
        Some(format!("cap-{}", cap - 2).as_str())
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_reassigns_waiters_and_tasks_to_immutable_old_owner() {
    let (core, mock, tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let original_main = session.main_agent().unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(500),
        text: "detached".into(),
    });
    session
        .submit_user_message("run pending work".into())
        .await
        .unwrap();
    wait_until(|| original_main.is_running()).await;

    let permission = pacode_types::PermissionRequest {
        id: pacode_types::PermissionId::generate(),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: CallId::generate(),
        tool: "write".into(),
        title: "Write owned.txt".into(),
        detail: "content".into(),
        risk: None,
        created_at_ms: pacode_types::now_ms(),
    };
    let _permission_waiter = session
        .permissions
        .register(permission.clone())
        .expect("permission should register");
    let question = pacode_types::question::Question::new(
        pacode_types::QuestionId::generate(),
        pacode_types::question::QuestionOrigin::new(AgentId::main(), "main", CallId::generate()),
        "Pick",
        "Pick one",
        vec![
            pacode_types::question::QuestionOption::new("yes", ""),
            pacode_types::question::QuestionOption::new("no", ""),
        ],
        false,
        pacode_types::now_ms(),
    )
    .unwrap();
    let _question_waiter = session.questions.register(question.clone()).unwrap();
    let task = session
        .tasks
        .spawn(TaskSpec::new(
            session.id.clone(),
            AgentId::main(),
            "sleep 5",
            tmp.path(),
        ))
        .await
        .unwrap();
    session.store.upsert_task(&task).await.unwrap();

    let detached_id = session.detach_main_turn().await.unwrap();
    let fresh_main = session.main_agent().unwrap();
    assert!(Arc::ptr_eq(
        &session.agent_for_host(&AgentId::main()).unwrap(),
        &fresh_main
    ));
    assert!(Arc::ptr_eq(
        &session.agent_for_host(&detached_id).unwrap(),
        &original_main
    ));
    assert_eq!(session.permissions.pending()[0].agent, detached_id);
    assert_eq!(session.questions.pending()[0].agent, detached_id);
    assert_eq!(session.tasks.info(&task.id).unwrap().owner, detached_id);
    assert_eq!(
        session
            .store
            .list_tasks(&session.id)
            .await
            .unwrap()
            .into_iter()
            .find(|info| info.id == task.id)
            .unwrap()
            .owner,
        detached_id
    );
    let (next_item_seq, expected_next) = {
        let main = session.main_agent().unwrap();
        let transcript = main.transcript.lock().unwrap();
        // Detach reserves the band reserved for store history, so the counter
        // starts there rather than at 0 even when the tail is empty.
        let floor = transcript
            .tail
            .iter()
            .map(|item| item.seq.saturating_add(1))
            .max()
            .unwrap_or(crate::transcript::HISTORY_ITEM_BASE);
        (transcript.next_item_seq, floor)
    };
    assert_eq!(next_item_seq, expected_next);

    let host = crate::host::SessionHost {
        session: session.clone(),
        agent: AgentId::main(),
    };
    let after = host
        .spawn_task(TaskSpec::new(
            session.id.clone(),
            AgentId::main(),
            "sleep 5",
            tmp.path(),
        ))
        .await
        .unwrap();
    assert_eq!(session.tasks.info(&after).unwrap().owner, AgentId::main());
    session.tasks.kill(&after).await.unwrap();
    session.stop_agent(&detached_id).await.unwrap();
    session.tasks.kill(&task.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_session_rejects_prepared_detach_commit() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(200),
        text: "detached".into(),
    });
    session
        .submit_user_message("detach while open".into())
        .await
        .unwrap();
    let main = session.main_agent().unwrap();
    wait_until(|| main.is_running()).await;
    let plan = session.prepare_detach(&main);

    session
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = session.commit_detach(&main, plan).unwrap_err();
    assert!(error.to_string().contains("closed"));
    assert!(Arc::ptr_eq(&session.main_agent().unwrap(), &main));
    assert!(main.id().is_main());
    assert_eq!(session.agents.read().unwrap().len(), 1);

    session.stop_agent(&AgentId::main()).await.unwrap();
    wait_until(|| !main.is_running()).await;
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_detach_with_no_turn_running_fails() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    let err = session.detach_main_turn().await.unwrap_err();
    assert!(
        err.to_string().contains("no turn is running"),
        "expected error about no turn running, got: {err}"
    );

    let reply = core.handle(&session_id, Request::DetachTurn).await;
    match reply {
        Reply::Error { message } => {
            assert!(
                message.contains("no turn is running"),
                "expected error message about no turn running, got: {message}"
            );
        }
        other => panic!("expected Reply::Error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_prompt_and_fork_history_survive_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("subagent-prompt.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = first_core.session(&session_id).unwrap();
    mock.push(MockResponse::Text("parent answer".into()));
    session
        .submit_user_message("parent prompt".into())
        .await
        .unwrap();
    wait_until(|| {
        session
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "parent answer")
    })
    .await;
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "sub answer".into(),
    });
    let sub_id = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("forked".into()),
            prompt: "fork prompt".into(),
            tools: None,
            exclude_tools: None,
            fork: true,
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| sub.is_running()).await;
    let before = sub.history.lock().unwrap().clone();
    assert_eq!(before.next_seq, 3);
    assert_eq!(
        before
            .rows()
            .iter()
            .map(|(seq, _)| *seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(sub.prompt(), Some("fork prompt".into()));
    wait_until(|| !sub.is_running()).await;
    first_core.shutdown().await;

    let second_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    let restored = second_core
        .session(&session_id)
        .unwrap()
        .agent(&sub_id)
        .unwrap();
    assert_eq!(restored.prompt(), Some("fork prompt".into()));
    let after = restored.history.lock().unwrap().clone();
    assert_eq!(after.next_seq, 4);
    assert_eq!(
        after.rows().iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_detach_with_live_subagents_succeeds() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Two live subagents must not block the detach: there is no live cap.
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "subagent 1 working...".into(),
    });
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "subagent 2 working...".into(),
    });

    let _sub1 = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("sub1".into()),
            prompt: "work 1".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: None,
        })
        .await
        .unwrap();

    let _sub2 = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("sub2".into()),
            prompt: "work 2".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: None,
        })
        .await
        .unwrap();

    // Start a turn on main
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "main turn...".into(),
    });
    session
        .submit_user_message("main work".into())
        .await
        .unwrap();

    // Detach succeeds regardless of how many subagents are live.
    let detached_id = session.detach_main_turn().await.unwrap();
    assert!(!detached_id.is_main());
}

/// There is no concurrency cap on subagents: spawning more than the old
/// `agents.max_live` default (8) must succeed for every spawn.
#[tokio::test(flavor = "multi_thread")]
async fn test_spawn_agent_has_no_live_cap() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    const SPAWNS: usize = 10;
    for i in 0..SPAWNS {
        mock.push(MockResponse::Slow {
            delay: Duration::from_millis(300),
            text: format!("subagent {i} working..."),
        });
    }

    let mut ids = Vec::new();
    for i in 0..SPAWNS {
        let id = session
            .spawn_agent(pacode_tools::AgentSpec {
                name: Some(format!("sub{i}")),
                prompt: format!("work {i}"),
                tools: None,
                exclude_tools: None,
                fork: false,
                model: None,
                effort: None,
            })
            .await;
        assert!(id.is_ok(), "spawn {i} failed: {:?}", id.err());
        ids.push(id.unwrap());
    }

    assert_eq!(ids.len(), SPAWNS);
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), SPAWNS, "spawned agent ids must be unique");
}

/// The W128 target, as a single session: a hundred and twenty-eight subagents
/// must all come up, and they must come up fast. The two halves fail
/// differently, so they are checked separately.
///
/// Speed is checked as growth, not as a wall-clock number, because a fixed
/// threshold would be a flaky test on a loaded machine. What must not happen is
/// spawn *N* costing more than spawn 1 by a factor that climbs with *N* — the
/// shape an O(n²) registry, a per-spawn full scan, or a store write per agent
/// leaves behind. Half again the mean is a ceiling that still catches quadratic
/// growth (128× would be far past it) and leaves headroom for a slow disk.
///
/// The prompt is deliberately not `fork: true`: forking copies the parent's
/// whole history into the child, which is a different cost from creating the
/// agent, and mixing the two would make a slow run unattributable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_spawns_128_subagents_without_quadratic_slowdown() {
    const SPAWNS: usize = 128;
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Every agent gets a slow provider so none of them finishes (and frees its
    // slot) while the fleet is being built: the test measures creation under
    // the load it is meant to survive.
    for index in 0..SPAWNS {
        mock.push(MockResponse::Slow {
            delay: Duration::from_secs(30),
            text: format!("subagent {index} working"),
        });
    }

    let mut ids = Vec::with_capacity(SPAWNS);
    let mut first_quartile = std::time::Duration::ZERO;
    let mut last_quartile = std::time::Duration::ZERO;
    let started = std::time::Instant::now();
    for index in 0..SPAWNS {
        let tick = std::time::Instant::now();
        let id = session
            .spawn_agent(pacode_tools::AgentSpec {
                name: Some(format!("w{index}")),
                prompt: format!("work {index}"),
                tools: None,
                exclude_tools: None,
                fork: false,
                model: None,
                effort: None,
            })
            .await;
        assert!(id.is_ok(), "spawn {index} failed: {:?}", id.err());
        ids.push(id.unwrap());
        let elapsed = tick.elapsed();
        if index < SPAWNS / 4 {
            first_quartile += elapsed;
        } else if index >= SPAWNS * 3 / 4 {
            last_quartile += elapsed;
        }
    }
    let total = started.elapsed();

    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        SPAWNS,
        "every spawn must produce its own agent id"
    );
    {
        // The map holds the main agent alongside the fleet, so the count is one
        // larger than the number of spawns.
        let agents = session.agents.read().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            agents.len(),
            SPAWNS + 1,
            "all {SPAWNS} subagents must be live in the session at once"
        );
    }

    let per_first = first_quartile / (SPAWNS / 4) as u32;
    let per_last = last_quartile / (SPAWNS - SPAWNS * 3 / 4) as u32;
    assert!(
        per_last <= per_first * 3 / 2,
        "spawning got slower as the session filled: first {per_first:?} per spawn, \
         last {per_last:?} per spawn ({} total). That is what an O(n^2) registry or a \
         per-spawn full scan looks like",
        humantime(total)
    );
}

/// A short, readable rendering of a duration for failure messages.
fn humantime(duration: std::time::Duration) -> String {
    format!("{:.1?}", duration)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_detach_turn_events_carry_new_id_and_none_carry_main() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Slow response so we can detach mid-turn
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(50),
        text: "word1 word2 word3 word4 word5".into(),
    });

    let mut rx = core.subscribe(&session_id).unwrap();

    session
        .submit_user_message("start long work".into())
        .await
        .unwrap();

    // Wait until turn starts and first delta arrives. Each wait gets its own
    // deadline so a slow first leg cannot starve the second one.
    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for initial text delta"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TextDelta { .. })) = res {
                    break;
                }
            }
        }
    }

    // Now detach the turn!
    let reply = core.handle(&session_id, Request::DetachTurn).await;
    let detached_id = match reply {
        Reply::TurnDetached { agent } => agent,
        other => panic!("expected TurnDetached, got {other:?}"),
    };
    assert!(!detached_id.is_main());

    // Collect subsequent events until TurnEnded
    let mut saw_subsequent_delta = false;
    let mut saw_turn_ended = false;

    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => break,
            res = rx.recv() => {
                if let Ok((_seq, event)) = res {
                    match event {
                        Event::TextDelta { agent, .. } => {
                            // Any subsequent delta must NOT carry main
                            assert_eq!(agent, detached_id, "delta after detach must carry detached_id");
                            saw_subsequent_delta = true;
                        }
                        Event::TurnEnded { agent, stop, .. } if agent == detached_id => {
                            assert_eq!(stop, TurnStop::Completed);
                            saw_turn_ended = true;
                            break;
                        }
                        Event::TurnEnded { agent, .. } if agent.is_main() => {
                            panic!("turn ended event must not be emitted for main agent after detach");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    assert!(saw_subsequent_delta || saw_turn_ended);
    assert!(saw_turn_ended);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_fresh_main_history_equals_conversation_as_it_stood_before_detached_turn() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Turn 1 completes normally on main
    mock.push(MockResponse::Text("Response 1".into()));
    session
        .submit_user_message("Message 1".into())
        .await
        .unwrap();

    let mut rx = core.subscribe(&session_id).unwrap();

    // Each wait gets its own deadline: one shared budget across three waits
    // made this test fail under load, since a slow first turn left the later
    // ones with whatever time was left over.
    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for Turn 1 to complete"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent.is_main()
                {
                    break;
                }
            }
        }
    }

    let main_before = session.main_agent().unwrap();
    let hist_before = main_before.history.lock().unwrap().messages.clone();
    assert_eq!(hist_before.len(), 2);
    assert_eq!(hist_before[0].text(), "Message 1");
    assert_eq!(hist_before[1].text(), "Response 1");

    // Turn 2 begins on main and is detached
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(50),
        text: "Long response to message 2".into(),
    });
    session
        .submit_user_message("Message 2 (to be detached)".into())
        .await
        .unwrap();

    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for turn 2 delta"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TextDelta { .. })) = res {
                    break;
                }
            }
        }
    }

    let detached_id = session.detach_main_turn().await.unwrap();
    assert!(!detached_id.is_main());

    // Verify fresh main agent's history
    let fresh_main = session.main_agent().unwrap();
    let fresh_hist = fresh_main.history.lock().unwrap().messages.clone();
    assert_eq!(
        fresh_hist.len(),
        2,
        "fresh main history must carry exactly the conversation before Turn 2"
    );
    assert_eq!(fresh_hist[0].text(), "Message 1");
    assert_eq!(fresh_hist[1].text(), "Response 1");

    // Main agent is NOT running and has its own turn lock
    assert!(!fresh_main.is_running());

    // Send a new message on fresh main immediately; it must not block on the detached turn!
    mock.push(MockResponse::Text("Response 3 to fresh message".into()));
    session
        .submit_user_message("Message 3 on fresh main".into())
        .await
        .unwrap();

    let mut saw_main_turn_3_ended = false;
    while !saw_main_turn_3_ended {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for Turn 3 on fresh main"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent.is_main()
                {
                    saw_main_turn_3_ended = true;
                }
            }
        }
    }
    assert!(saw_main_turn_3_ended);

    let final_main_hist = session
        .main_agent()
        .unwrap()
        .history
        .lock()
        .unwrap()
        .messages
        .clone();
    assert_eq!(final_main_hist.len(), 4);
    assert_eq!(final_main_hist[0].text(), "Message 1");
    assert_eq!(final_main_hist[1].text(), "Response 1");
    assert_eq!(final_main_hist[2].text(), "Message 3 on fresh main");
    assert_eq!(final_main_hist[3].text(), "Response 3 to fresh message");
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_copies_persisted_pending_steer_with_original_seq() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("block_tool".into(), serde_json::json!({}))],
    });
    session.submit_user_message("first".into()).await.unwrap();
    wait_until(|| main.status() == AgentStatus::RunningTool).await;
    session
        .submit_user_message("pending steer".into())
        .await
        .unwrap();

    let detached_id = session.detach_main_turn().await.unwrap();
    let stored = session
        .store
        .load_messages(&session.id, &detached_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[0].seq, 0);
    assert_eq!(stored[0].message.text(), "first");
    assert_eq!(stored[1].seq, 1);
    assert!(stored[1].message.has_tool_uses());
    assert_eq!(stored[2].seq, 2);
    assert_eq!(stored[2].message.text(), "pending steer");

    let fresh_main = session.main_agent().unwrap();
    {
        let fresh_history = fresh_main.history.lock().unwrap();
        assert!(fresh_history.messages.is_empty());
    }

    let detached = session.agent(&detached_id).unwrap();
    assert!(detached.has_pending_user_messages());
    session.stop_agent(&detached_id).await.unwrap();
    wait_until(|| !detached.is_running()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_upsert_failure_keeps_running_main_and_pending_reservations() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("block_tool".into(), serde_json::json!({}))],
    });
    session
        .submit_user_message("running prompt".into())
        .await
        .unwrap();
    let main = session.main_agent().unwrap();
    wait_until(|| main.status() == AgentStatus::RunningTool).await;
    session
        .submit_user_message("pending steer".into())
        .await
        .unwrap();

    let main_id = main.id();
    let history = main.history.lock().unwrap().clone();
    let next_seq = history.next_seq;
    let baseline = main
        .history_before_turn
        .lock()
        .unwrap()
        .as_ref()
        .map(|history| (history.messages.len(), history.next_seq));
    let prompt = main.prompt();
    let pending_seq = main.persisted_pending_user_messages()[0].0;
    session.store.set_fault_injection(true);

    let error = session.detach_main_turn().await.unwrap_err();
    assert!(error.to_string().contains("store"), "got: {error}");
    let current = session.main_agent().unwrap();
    assert!(Arc::ptr_eq(&current, &main));
    assert_eq!(current.id(), main_id);
    assert!(current.is_running());
    assert_eq!(current.history.lock().unwrap().next_seq, next_seq);
    assert!(current.has_pending_user_messages());
    assert_eq!(current.persisted_pending_user_messages()[0].0, pending_seq);
    assert_eq!(
        current
            .history_before_turn
            .lock()
            .unwrap()
            .as_ref()
            .map(|history| (history.messages.len(), history.next_seq)),
        baseline
    );
    assert_eq!(current.prompt(), prompt);
    assert_eq!(session.agents.read().unwrap().len(), 1);

    session.store.set_fault_injection(false);
    session.stop_agent(&main_id).await.unwrap();
    wait_until(|| !main.is_running()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_store_failure_keeps_main_state_and_pending_seq() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("block_tool".into(), serde_json::json!({}))],
    });
    session
        .submit_user_message("running prompt".into())
        .await
        .unwrap();
    let main = session.main_agent().unwrap();
    wait_until(|| main.status() == AgentStatus::RunningTool).await;
    session
        .submit_user_message("pending steer".into())
        .await
        .unwrap();
    let next_seq = main.history.lock().unwrap().next_seq;
    let pending_seq = main.persisted_pending_user_messages()[0].0;
    session.store.set_fault_injection(true);

    assert!(session.detach_main_turn().await.is_err());
    let current = session.main_agent().unwrap();
    assert!(Arc::ptr_eq(&current, &main));
    assert!(current.id().is_main());
    assert!(current.is_running());
    assert_eq!(current.history.lock().unwrap().next_seq, next_seq);
    assert!(current.has_pending_user_messages());
    assert_eq!(current.persisted_pending_user_messages()[0].0, pending_seq);
    assert_eq!(session.agents.read().unwrap().len(), 1);

    session.store.set_fault_injection(false);
    session.stop_agent(&AgentId::main()).await.unwrap();
    wait_until(|| !main.is_running()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_orders_waiting_submit_on_fresh_main() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut events = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(500),
        text: "detached answer".into(),
    });
    mock.push(MockResponse::Text("fresh answer".into()));
    session
        .submit_user_message("before detach".into())
        .await
        .unwrap();
    let original_main = session.main_agent().unwrap();
    wait_until(|| original_main.is_running()).await;

    let submit_guard = session.submit_lock.lock().await;
    let detach = tokio::spawn({
        let session = session.clone();
        async move { session.detach_main_turn().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let submit = tokio::spawn({
        let session = session.clone();
        async move { session.submit_user_message("after detach".into()).await }
    });
    drop(submit_guard);

    let detached_id = detach.await.unwrap().unwrap();
    session.stop_agent(&detached_id).await.unwrap();
    submit.await.unwrap().unwrap();

    assert_eq!(wait_main_turn_end(&mut events).await, TurnStop::Completed);
    let fresh_main = session.main_agent().unwrap();
    assert!(!Arc::ptr_eq(&fresh_main, &original_main));
    assert!(
        fresh_main
            .history
            .lock()
            .unwrap()
            .messages
            .iter()
            .any(|message| message.text() == "after detach")
    );
    assert!(
        !original_main
            .history
            .lock()
            .unwrap()
            .messages
            .iter()
            .any(|message| message.text() == "after detach")
    );
    wait_until(|| !original_main.is_running()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_detached_agent_reaches_terminal_status_when_finished() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(30),
        text: "Finished answer from subagent".into(),
    });

    let mut rx = core.subscribe(&session_id).unwrap();
    session
        .submit_user_message("task to detach".into())
        .await
        .unwrap();

    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for stream start"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TextDelta { .. })) = res {
                    break;
                }
            }
        }
    }

    let detached_id = session.detach_main_turn().await.unwrap();

    // Wait until the detached agent turn completes
    let mut saw_agent_updated_finished = false;
    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for detached agent to finish"),
            res = rx.recv() => {
                if let Ok((_seq, event)) = res {
                    match event {
                        Event::AgentUpdated(info) if info.id == detached_id => {
                            if info.status == AgentStatus::Finished {
                                saw_agent_updated_finished = true;
                            }
                        }
                        Event::TurnEnded { agent, stop, .. } if agent == detached_id => {
                            assert_eq!(stop, TurnStop::Completed);
                            if saw_agent_updated_finished {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    assert!(saw_agent_updated_finished);
    let detached_agent = session.agent(&detached_id).unwrap();
    assert_eq!(detached_agent.info().status, AgentStatus::Finished);
}

/// Point 4 test:
/// Assistant messages and tool results are appended ONLY after stream/step completion,
/// NOT while streaming.
/// This test verifies that while a turn is streaming:
/// - The assistant message has NOT been added to agent.history
/// - The assistant message has NOT been written to store
/// - Only upon stream completion does the assistant message appear in history and store.
#[tokio::test(flavor = "multi_thread")]
async fn test_point_4_assistant_messages_appended_only_at_completion() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(50),
        text: "tokenA tokenB tokenC".into(),
    });

    let mut rx = core.subscribe(&session_id).unwrap();
    session
        .submit_user_message("streaming test".into())
        .await
        .unwrap();

    // Wait until first delta arrives (mid-stream)
    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for first delta"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TextDelta { .. })) = res {
                    break;
                }
            }
        }
    }

    // CHECK POINT 4 DURING STREAMING:
    // The running main agent's history contains ONLY the user message (len = 1);
    // the assistant message has NOT been appended yet!
    let main_agent = session.main_agent().unwrap();
    {
        let hist = main_agent.history.lock().unwrap();
        assert_eq!(
            hist.messages.len(),
            1,
            "mid-stream: assistant message must NOT be in history yet"
        );
        assert_eq!(hist.messages[0].role, Role::User);
    }

    // Check store during streaming:
    let stored_msgs = session
        .store
        .load_messages(&session.id, &main_agent.id())
        .await
        .unwrap();
    assert_eq!(
        stored_msgs.len(),
        1,
        "mid-stream: store must contain only the user message under main"
    );
    assert_eq!(stored_msgs[0].message.role, Role::User);

    // Wait for the turn to complete
    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for turn completion"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent.is_main()
                {
                    break;
                }
            }
        }
    }

    // After completion, the assistant message IS appended
    {
        let hist = main_agent.history.lock().unwrap();
        assert_eq!(hist.messages.len(), 2);
        assert_eq!(hist.messages[1].role, Role::Assistant);
    }
    let stored_msgs_after = session
        .store
        .load_messages(&session.id, &main_agent.id())
        .await
        .unwrap();
    assert_eq!(stored_msgs_after.len(), 2);
    assert_eq!(stored_msgs_after[1].message.role, Role::Assistant);
}

/// `set_model` must reach the turn loop: it rewrites the session meta, the main
/// agent's `AgentInfo`, and every live subagent that inherited the old session
/// model (`spec.model = None`). A subagent spawned with an explicit model keeps
/// it, and each updated info lands in the store for the resume path.
#[tokio::test(flavor = "multi_thread")]
async fn test_set_model_propagates_to_agents() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Slow responses keep both subagents live while set_model runs.
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "inherited subagent working".into(),
    });
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "pinned subagent working".into(),
    });

    let inherited_id = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("inherited".into()),
            prompt: "work".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: None,
        })
        .await
        .unwrap();

    let pinned_route = ModelRoute::new("mock", "pinned-model");
    let pinned_id = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("pinned".into()),
            prompt: "work".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: Some(pinned_route.clone()),
            effort: None,
        })
        .await
        .unwrap();

    let new_route = ModelRoute::new("mock", "new-model");
    session.set_model(new_route.clone()).await.unwrap();

    assert_eq!(session.meta().model, new_route);
    assert_eq!(session.main_agent().unwrap().info().model, new_route);
    assert_eq!(
        session.agent(&inherited_id).unwrap().info().model,
        new_route,
        "subagent that inherited the session model follows it"
    );
    assert_eq!(
        session.agent(&pinned_id).unwrap().info().model,
        pinned_route,
        "subagent spawned with an explicit model keeps it"
    );

    // The resume path reads agent rows back, so the update must be persisted.
    let stored = session.store.list_agents(&session.id).await.unwrap();
    assert_eq!(
        stored.iter().find(|a| a.id.is_main()).unwrap().model,
        new_route
    );
    assert_eq!(
        stored.iter().find(|a| a.id == inherited_id).unwrap().model,
        new_route
    );
    assert_eq!(
        stored.iter().find(|a| a.id == pinned_id).unwrap().model,
        pinned_route
    );
}

/// A route to a provider this session cannot reach is rejected before anything
/// is mutated.
#[tokio::test(flavor = "multi_thread")]
async fn test_set_model_rejects_unknown_provider() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let original = session.meta().model;

    let err = session
        .set_model(ModelRoute::new("nope", "some-model"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("unknown provider"),
        "expected unknown-provider error, got: {err}"
    );

    assert_eq!(session.meta().model, original);
    assert_eq!(session.main_agent().unwrap().info().model, original);

    let reply = core
        .handle(
            &session_id,
            Request::SetModel(ModelRoute::new("nope", "some-model")),
        )
        .await;
    match reply {
        Reply::Error { message } => assert!(message.contains("unknown provider")),
        other => panic!("expected Reply::Error, got {other:?}"),
    }
}

/// `set_effort` propagates exactly like `set_model`: main plus live subagents
/// still on the old session effort; a pinned-effort subagent keeps its own.
#[tokio::test(flavor = "multi_thread")]
async fn test_set_effort_propagates_to_agents() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "inherited subagent working".into(),
    });
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "pinned subagent working".into(),
    });

    let inherited_id = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("inherited".into()),
            prompt: "work".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: None,
        })
        .await
        .unwrap();

    let pinned_id = session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("pinned".into()),
            prompt: "work".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: Some(Effort::Low),
        })
        .await
        .unwrap();

    session.set_effort(Effort::High).await;

    assert_eq!(session.meta().effort, Effort::High);
    assert_eq!(session.main_agent().unwrap().info().effort, Effort::High);
    assert_eq!(
        session.agent(&inherited_id).unwrap().info().effort,
        Effort::High,
        "subagent that inherited the session effort follows it"
    );
    assert_eq!(
        session.agent(&pinned_id).unwrap().info().effort,
        Effort::Low,
        "subagent spawned with an explicit effort keeps it"
    );
}

/// End-to-end seam: the next turn's `CompletionRequest` carries the model and
/// effort set mid-session.
#[tokio::test(flavor = "multi_thread")]
async fn test_turn_after_set_model_uses_new_route() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    session
        .set_model(ModelRoute::new("mock", "switched-model"))
        .await
        .unwrap();
    session.set_effort(Effort::Max).await;

    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Text("ok".into()));
    session.submit_user_message("hi".into()).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent.is_main()
                {
                    break;
                }
            }
        }
    }

    let req = mock.requests().last().unwrap().clone();
    assert_eq!(req.model, "switched-model");
    assert_eq!(req.effort, Some(Effort::Max));
}

/// Poll `f` until it holds or 2 s elapse (turn unwinding is async).
async fn wait_until(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within 2s");
}

/// Wire-shape assertion: every assistant `tool_use` in a request's message
/// list must be answered by a `tool_result` in the `Role::Tool` run right
/// after it — the invariant providers 400 on when broken.
fn assert_no_orphan_tool_uses(messages: &[Arc<Message>]) {
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != Role::Assistant {
            i += 1;
            continue;
        }
        let uses: Vec<CallId> = messages[i]
            .tool_uses()
            .map(|(id, _, _)| id.clone())
            .collect();
        i += 1;
        let mut answered: Vec<CallId> = Vec::new();
        while i < messages.len() && messages[i].role == Role::Tool {
            for block in &messages[i].content {
                if let ContentBlock::ToolResult { call_id, .. } = block {
                    answered.push(call_id.clone());
                }
            }
            i += 1;
        }
        for id in uses {
            assert!(
                answered.contains(&id),
                "tool_use {id} has no tool_result in the request"
            );
        }
    }
}

/// Wait for the main agent's next `TurnEnded` (3 s budget).
async fn wait_main_turn_end(rx: &mut tokio::sync::broadcast::Receiver<(u64, Event)>) -> TurnStop {
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent.is_main()
                {
                    return stop;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_user_message_id_does_not_append_or_start_another_turn() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(200),
        text: "one answer".into(),
    });
    let request = Request::UserMessage {
        text: "send once".into(),
        images: Vec::new(),
        message_id: Some("msg-duplicate".into()),
    };

    let (first, second) = tokio::join!(
        core.handle(&session_id, request.clone()),
        core.handle(&session_id, request)
    );
    assert!(matches!(first, Reply::Ok));
    assert!(matches!(second, Reply::Ok));

    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    assert_eq!(
        main.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(
        main.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail
            .iter()
            .filter(|item| matches!(&item.kind, TranscriptKind::User { .. }))
            .count(),
        1
    );
    assert_eq!(wait_main_turn_end(&mut rx).await, TurnStop::Completed);
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reused_user_message_id_with_different_text_is_rejected() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Text("first answer".into()));
    assert!(matches!(
        core.handle(
            &session_id,
            Request::UserMessage {
                text: "first".into(),
                images: Vec::new(),
                message_id: Some("msg-conflict".into()),
            },
        )
        .await,
        Reply::Ok
    ));
    assert_eq!(wait_main_turn_end(&mut rx).await, TurnStop::Completed);

    let reply = core
        .handle(
            &session_id,
            Request::UserMessage {
                text: "second".into(),
                images: Vec::new(),
                message_id: Some("msg-conflict".into()),
            },
        )
        .await;
    assert!(matches!(
        reply,
        Reply::Error { ref message } if message.contains("different content")
    ));
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    assert_eq!(
        main.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_user_message_submission_releases_its_id_for_retry() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    session
        .meta
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .zdr = true;
    let request = Request::UserMessage {
        text: "retry after rejection".into(),
        images: Vec::new(),
        message_id: Some("msg-retry".into()),
    };

    let reply = core.handle(&session_id, request.clone()).await;
    assert!(matches!(
        reply,
        Reply::Error { ref message } if message.contains("zdr_blocked")
    ));
    session
        .meta
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .zdr = false;
    mock.push(MockResponse::Text("accepted".into()));
    let mut rx = core.subscribe(&session_id).unwrap();
    assert!(matches!(core.handle(&session_id, request).await, Reply::Ok));
    assert_eq!(wait_main_turn_end(&mut rx).await, TurnStop::Completed);
    assert_eq!(
        session
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_user_message_append_rolls_back_reservation_and_retry_succeeds() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut events = core.subscribe(&session_id).unwrap();
    let request = Request::UserMessage {
        text: "retry after append failure".into(),
        images: Vec::new(),
        message_id: Some("msg-append-retry".into()),
    };

    session.store.delete_session(&session.id).await.unwrap();
    let reply = core.handle(&session_id, request.clone()).await;
    assert!(matches!(
        reply,
        Reply::Error { ref message } if message.contains("store")
    ));

    let main = session.main_agent().unwrap();
    {
        let history = main
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(history.messages.is_empty());
        assert_eq!(history.next_seq, 0);
        assert_eq!(history.estimated_tokens, 0);
    }
    assert!(!main.has_pending_user_messages());
    assert!(main.injections.is_empty());
    assert!(
        main.history_before_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none()
    );
    assert!(!main.is_running());
    assert!(mock.requests().is_empty());
    assert!(
        !main
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail
            .iter()
            .any(|item| matches!(&item.kind, TranscriptKind::User { .. }))
    );

    tokio::task::yield_now().await;
    while let Ok((_seq, event)) = events.try_recv() {
        assert!(!matches!(
            event,
            Event::ItemAdded(_) | Event::TurnStarted { .. }
        ));
    }

    let reservations = core.user_message_ids.read().await.get(&session_id).cloned();
    if let Some(reservations) = reservations {
        assert!(reservations.lock().await.is_empty());
    }

    let meta = session.meta();
    session.store.upsert_session(&meta).await.unwrap();
    mock.push(MockResponse::Text("accepted after recovery".into()));
    assert!(matches!(core.handle(&session_id, request).await, Reply::Ok));
    assert_eq!(wait_main_turn_end(&mut events).await, TurnStop::Completed);

    {
        let history = main
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(history.messages.len(), 2);
        assert_eq!(history.messages[0].role, Role::User);
        assert_eq!(history.messages[1].role, Role::Assistant);
    }
    assert!(!main.has_pending_user_messages());
    assert_eq!(mock.requests().len(), 1);
    let stored = session
        .store
        .load_messages(&session.id, &main.id())
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].message.role, Role::User);
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_store_failure_does_not_mutate_history() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    {
        let mut history = main.history.lock().unwrap();
        for index in 0..10 {
            history.push(Message::user(format!("history {index}")));
        }
    }
    let before = main.history.lock().unwrap().clone();
    mock.push(MockResponse::Text("summary".into()));
    session.store.set_fault_injection(true);

    assert!(crate::compaction::compact(&session, &main).await.is_err());
    session.store.set_fault_injection(false);
    let after = main.history.lock().unwrap().clone();
    assert_eq!(after.summary, before.summary);
    assert_eq!(after.summary_upto, before.summary_upto);
    assert_eq!(after.rows(), before.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn session_host_persists_task_started_and_terminal_rows() {
    let (core, _mock, tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let host = crate::host::SessionHost {
        session: session.clone(),
        agent: AgentId::main(),
    };
    let task = host
        .spawn_task(TaskSpec::new(
            session_id.clone(),
            AgentId::main(),
            "printf done",
            tmp.path(),
        ))
        .await
        .unwrap();
    let running = session
        .store
        .list_tasks(&session_id)
        .await
        .unwrap()
        .into_iter()
        .find(|info| info.id == task)
        .unwrap();
    assert_eq!(running.status, pacode_types::TaskStatus::Running);
    host.wait_task(&task, Duration::from_secs(5), false).await;
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("task terminal row was not persisted"),
            _ = tokio::task::yield_now() => {
                if session
                    .store
                    .list_tasks(&session_id)
                    .await
                    .ok()
                    .and_then(|tasks| tasks.into_iter().find(|info| info.id == task))
                    .is_some_and(|info| info.status.is_terminal())
                {
                    break;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn close_reopen_keeps_task_router_tombstone() {
    let (core, _mock, tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let task = session
        .tasks
        .spawn(TaskSpec::new(
            session_id.clone(),
            AgentId::main(),
            "sleep 5",
            tmp.path(),
        ))
        .await
        .unwrap();
    session.store.upsert_task(&task).await.unwrap();
    core.close_session(&session_id).await;
    assert!(core.is_task_tombstoned(&task.id));
    core.open_session(Attach::Resume {
        session: session_id.clone(),
    })
    .await
    .unwrap();
    assert!(core.is_task_tombstoned(&task.id));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_hydrates_terminal_tasks_from_store() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("task-hydration.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = first_core.session(&session_id).unwrap();
    let task = session
        .tasks
        .spawn(TaskSpec::new(
            session_id.clone(),
            AgentId::main(),
            "printf persisted",
            temp_dir.path(),
        ))
        .await
        .unwrap();
    session.store.upsert_task(&task).await.unwrap();
    session
        .tasks
        .wait(&task.id, Duration::from_secs(5), false)
        .await;
    let ended = session.tasks.info(&task.id).unwrap();
    assert!(ended.status.is_terminal());
    session.store.upsert_task(&ended).await.unwrap();
    first_core.shutdown().await;

    let second_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    let restored = second_core
        .session(&session_id)
        .unwrap()
        .tasks_of_session()
        .into_iter()
        .find(|info| info.id == task.id)
        .unwrap();
    assert_eq!(restored.status, ended.status);
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_admission_queue_is_bounded_and_deduplicated() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let agent = AgentId::main();
    let admissions = (0..crate::session::MAX_RECOVERY_ADMISSIONS + 17)
        .map(|index| format!("bounded-{index}"))
        .collect::<Vec<_>>();

    session.queue_recovery_admissions(&agent, admissions);
    {
        let queue = session
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(queue.len(), crate::session::MAX_RECOVERY_ADMISSIONS);
        assert_eq!(queue.front().unwrap().1, "bounded-0");
        assert_eq!(
            queue.back().unwrap().1,
            format!("bounded-{}", crate::session::MAX_RECOVERY_ADMISSIONS - 1)
        );
    }
    session.queue_recovery_admissions(&agent, ["bounded-0".to_string()]);
    assert_eq!(
        session
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        crate::session::MAX_RECOVERY_ADMISSIONS
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_admission_redrives_after_store_commit_before_dispatch() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let store = session.store.clone();
    core.close_session(&session_id).await;

    let message = Message::user("recover after crash");
    assert_eq!(
        store
            .append_user_message_with_turn(
                &session_id,
                &AgentId::main(),
                0,
                "msg-crash-window",
                41,
                Some("trn_msg-crash-window"),
                &message,
            )
            .await
            .unwrap(),
        pacode_store::MessageAdmissionResult::Appended
    );

    mock.push(MockResponse::Text("recovered answer".into()));
    core.open_session(Attach::Resume {
        session: session_id.clone(),
    })
    .await
    .unwrap();

    wait_until(|| !mock.requests().is_empty()).await;
    wait_until(|| {
        core.session(&session_id)
            .and_then(|session| session.main_agent())
            .is_some_and(|agent| {
                agent
                    .history
                    .lock()
                    .unwrap()
                    .messages
                    .iter()
                    .any(|message| {
                        message.role == Role::Assistant && message.text() == "recovered answer"
                    })
            })
    })
    .await;
    let admission = store
        .get_message_admission(&session_id, "msg-crash-window")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        admission.state,
        pacode_store::MessageAdmissionState::Terminal
    );
    assert_eq!(
        store
            .load_messages(&session_id, &AgentId::main())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_recovery_redrives_all_pending_admissions() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let store = core.session(&session_id).unwrap().store.clone();
    core.close_session(&session_id).await;
    for (seq, message_id, text) in [
        (0, "msg-recovery-one", "recover one"),
        (1, "msg-recovery-two", "recover two"),
    ] {
        store
            .append_user_message_with_turn(
                &session_id,
                &AgentId::main(),
                seq,
                message_id,
                seq + 100,
                Some(&format!("trn_{message_id}")),
                &Message::user(text),
            )
            .await
            .unwrap();
    }
    mock.push(MockResponse::Text("answer one".into()));
    mock.push(MockResponse::Text("answer two".into()));
    core.open_session(Attach::Resume {
        session: session_id.clone(),
    })
    .await
    .unwrap();

    wait_until(|| mock.requests().len() >= 2).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let first = store
            .get_message_admission(&session_id, "msg-recovery-one")
            .await
            .unwrap()
            .unwrap();
        let second = store
            .get_message_admission(&session_id, "msg-recovery-two")
            .await
            .unwrap()
            .unwrap();
        if first.state == pacode_store::MessageAdmissionState::Terminal
            && second.state == pacode_store::MessageAdmissionState::Terminal
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "admission recovery did not drain"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(mock.requests().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn internal_submit_creates_a_durable_admission() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "internal answer".into(),
    });
    session
        .submit_user_message("internal prompt".into())
        .await
        .unwrap();
    wait_until(|| !mock.requests().is_empty()).await;
    let admission = session
        .store
        .load_message_admissions()
        .await
        .unwrap()
        .into_iter()
        .find(|admission| admission.message_id.starts_with("internal-"))
        .expect("internal submit must have a durable admission");
    assert_eq!(
        admission.state,
        pacode_store::MessageAdmissionState::Running
    );
    wait_until(|| {
        session
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "internal answer")
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let admission = session
            .store
            .load_message_admissions()
            .await
            .unwrap()
            .into_iter()
            .find(|admission| admission.message_id.starts_with("internal-"))
            .expect("internal submit must have a durable admission");
        if admission.state == pacode_store::MessageAdmissionState::Terminal {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "internal admission did not become terminal"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn early_interrupt_fences_admission_before_progress_and_survives_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("early-interrupt.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = first_core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let message_id = "early-interrupt-admission";
    let turn = TurnId::new("trn_early-interrupt-admission");
    session
        .store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            message_id,
            1,
            Some(turn.as_str()),
            &Message::user("cancel before progress"),
        )
        .await
        .unwrap();
    assert!(main.begin_admission_turn(message_id.into(), turn.clone()));

    session.interrupt_all().await;

    assert!(main.admission_is_cancelled(message_id));
    let finish = main
        .finish_admission_turn(&turn, &TurnStop::Interrupted)
        .expect("fenced admission must finish through the supervisor path");
    assert!(finish.terminal);
    assert!(main.injections.is_empty());
    assert_eq!(
        session
            .store
            .get_message_admission(&session_id, message_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
    first_core.shutdown().await;

    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(mock.requests().is_empty());
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_exposes_turn_fences_and_clears_previous_agent_error() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "first answer".into(),
    });
    session
        .submit_user_message_with_images_idempotent(
            "first".into(),
            Vec::new(),
            "snapshot-fence-first".into(),
            1,
        )
        .await
        .unwrap();
    wait_until(|| session.snapshot().active_turn.is_some()).await;
    let active = session.snapshot();
    assert_eq!(
        active.active_turn,
        Some(TurnId::new("trn_snapshot-fence-first"))
    );
    assert!(active.turn_active);
    wait_until(|| session.snapshot().terminal_turn.is_some()).await;
    let terminal = session.snapshot();
    assert_eq!(
        terminal.terminal_turn,
        Some(TurnId::new("trn_snapshot-fence-first"))
    );
    assert!(!terminal.turn_active);
    assert_eq!(terminal.terminal_error, None);

    main.info
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error = Some("stale failure".into());
    mock.push(MockResponse::Text("second answer".into()));
    session
        .submit_user_message_with_images_idempotent(
            "second".into(),
            Vec::new(),
            "snapshot-fence-second".into(),
            2,
        )
        .await
        .unwrap();
    wait_until(|| {
        session
            .snapshot()
            .terminal_turn
            .is_some_and(|turn| turn == TurnId::new("trn_snapshot-fence-second"))
    })
    .await;
    assert_eq!(session.snapshot().terminal_error, None);
    assert_eq!(main.info().error, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn close_session_terminalizes_admissions_and_removes_runtime_owner() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let store = session.store.clone();
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            "close-durable-admission",
            1,
            Some("trn_close-durable-admission"),
            &Message::user("close me"),
        )
        .await
        .unwrap();

    assert!(matches!(
        core.handle(&session_id, Request::CloseSession).await,
        Reply::Ok
    ));
    assert!(core.session_ids().is_empty());
    assert_eq!(
        store
            .get_message_admission(&session_id, "close-durable-admission")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_pending_message_is_filtered_but_kept_in_audit() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("cancelled-message.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let store = first_core.session(&session_id).unwrap().store.clone();
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            "cancelled-pending",
            1,
            Some("trn_cancelled-pending"),
            &Message::user("do not resume this"),
        )
        .await
        .unwrap();
    store
        .mark_message_admission_terminal(&session_id, "cancelled-pending", Some("interrupted"))
        .await
        .unwrap();
    drop(store);
    drop(first_core);

    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    let session = second_core.session(&session_id).unwrap();
    assert!(
        !session
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "do not resume this")
    );
    assert_eq!(
        session
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap()
            .next_seq,
        1
    );
    assert_eq!(
        session
            .store
            .load_messages(&session_id, &AgentId::main())
            .await
            .unwrap()
            .len(),
        1
    );
    let admission = store_for_test(&second_core, &session_id, "cancelled-pending").await;
    assert_eq!(
        admission.state,
        pacode_store::MessageAdmissionState::Terminal
    );
    assert_eq!(admission.terminal_stop.as_deref(), Some("interrupted"));
    assert!(mock.requests().is_empty());
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_synthesizes_missing_tool_result_before_redrive() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission-tool-recovery.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let store = first_core.session(&session_id).unwrap().store.clone();
    let call_id = CallId::generate();
    let admission = "tool-recovery-admission";
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            admission,
            1,
            Some(&format!("trn_{admission}")),
            &Message::user("run tool"),
        )
        .await
        .unwrap();
    store
        .append_message(
            &session_id,
            &AgentId::main(),
            1,
            &Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "Cargo.toml"}),
                }],
            ),
        )
        .await
        .unwrap();
    store
        .mark_message_admission_running(&session_id, admission, &format!("trn_{admission}"))
        .await
        .unwrap();
    drop(store);
    drop(first_core);

    mock.push(MockResponse::Text("recovered tool answer".into()));
    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    wait_until(|| !mock.requests().is_empty()).await;
    let request = mock.requests().last().unwrap().clone();
    assert!(request.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { call_id: result, .. } if result == &call_id)
        })
    }));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if store_for_test(&second_core, &session_id, admission)
            .await
            .state
            == pacode_store::MessageAdmissionState::Terminal
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tool recovery did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_redrives_a_tool_round_without_a_final_assistant() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission-tool-round-recovery.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let store = first_core.session(&session_id).unwrap().store.clone();
    let call_id = CallId::generate();
    let admission = "tool-round-recovery-admission";
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            admission,
            1,
            Some(&format!("trn_{admission}")),
            &Message::user("run tool"),
        )
        .await
        .unwrap();
    store
        .append_message(
            &session_id,
            &AgentId::main(),
            1,
            &Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "Cargo.toml"}),
                }],
            ),
        )
        .await
        .unwrap();
    store
        .append_message(
            &session_id,
            &AgentId::main(),
            2,
            &Message::tool_result(call_id.clone(), "tool output", false),
        )
        .await
        .unwrap();
    store
        .mark_message_admission_running(&session_id, admission, &format!("trn_{admission}"))
        .await
        .unwrap();
    drop(store);
    drop(first_core);

    mock.push(MockResponse::Text("final answer after tools".into()));
    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();

    wait_until(|| !mock.requests().is_empty()).await;
    let request = mock.requests().last().unwrap().clone();
    assert!(request.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { call_id: result, .. } if result == &call_id)
        })
    }));
    assert_no_orphan_tool_uses(&request.messages);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if store_for_test(&second_core, &session_id, admission)
            .await
            .state
            == pacode_store::MessageAdmissionState::Terminal
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tool-round recovery did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crashed_admission_retries_without_terminalizing_queued_admissions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission-crash-retry.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let store = first_core.session(&session_id).unwrap().store.clone();
    let crashed = "crashed-admission";
    let queued = "queued-admission";
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            crashed,
            1,
            Some(&format!("trn_{crashed}")),
            &Message::user("crashed prompt"),
        )
        .await
        .unwrap();
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            1,
            queued,
            2,
            Some(&format!("trn_{queued}")),
            &Message::user("queued prompt"),
        )
        .await
        .unwrap();
    store
        .mark_message_admission_running(&session_id, crashed, &format!("trn_{crashed}"))
        .await
        .unwrap();
    drop(store);
    drop(first_core);

    mock.push(MockResponse::Text("retry answer".into()));
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "queued answer".into(),
    });
    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();

    wait_until(|| mock.requests().len() >= 2).await;
    let first = store_for_test(&second_core, &session_id, crashed).await;
    let second = store_for_test(&second_core, &session_id, queued).await;
    assert_eq!(first.state, pacode_store::MessageAdmissionState::Terminal);
    assert_ne!(second.state, pacode_store::MessageAdmissionState::Terminal);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let first = store_for_test(&second_core, &session_id, crashed).await;
        let second = store_for_test(&second_core, &session_id, queued).await;
        if first.state == pacode_store::MessageAdmissionState::Terminal
            && second.state == pacode_store::MessageAdmissionState::Terminal
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "admission retry did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(mock.requests().len(), 2);
    second_core.shutdown().await;
}

async fn store_for_test(
    core: &Core,
    session_id: &pacode_types::SessionId,
    message_id: &str,
) -> pacode_store::MessageAdmission {
    core.session(session_id)
        .unwrap()
        .store
        .get_message_admission(session_id, message_id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn detached_history_reloads_on_resume() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("detached-admission.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = first_core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "old detached".into(),
    });
    session
        .submit_user_message_with_images_idempotent(
            "recover detached".into(),
            Vec::new(),
            "msg-detached-recovery".into(),
            77,
        )
        .await
        .unwrap();
    wait_until(|| session.main_agent().unwrap().is_running()).await;
    wait_until(|| !mock.requests().is_empty()).await;
    let detached_id = session.detach_main_turn().await.unwrap();
    session.stop_agent(&detached_id).await.unwrap();
    wait_until(|| !session.agent(&detached_id).unwrap().is_running()).await;
    first_core.shutdown().await;

    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    let restored = second_core.session(&session_id).unwrap();
    assert!(
        !restored
            .agent(&detached_id)
            .unwrap()
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "recover detached")
    );
    assert!(
        restored
            .store
            .load_messages(&session_id, &detached_id)
            .await
            .unwrap()
            .iter()
            .any(|row| row.message.text() == "recover detached")
    );
    assert!(
        !restored
            .main_agent()
            .unwrap()
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "recover detached")
    );
    assert_eq!(
        restored
            .store
            .get_message_admission(&session_id, "msg-detached-recovery")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
    assert_eq!(mock.requests().len(), 1);
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_admission_retries_the_durable_detached_owner() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(200),
        text: "detached answer".into(),
    });
    session
        .submit_user_message_with_images_idempotent(
            "detach owner".into(),
            Vec::new(),
            "detach-owner-turn".into(),
            1,
        )
        .await
        .unwrap();
    wait_until(|| !mock.requests().is_empty()).await;
    let detached_id = session.detach_main_turn().await.unwrap();
    let fresh_main = session.main_agent().unwrap();
    let detached = session.agent(&detached_id).unwrap();
    let seq = detached.history.lock().unwrap().next_seq;
    session
        .store
        .append_user_message_with_turn(
            &session_id,
            &detached_id,
            seq,
            "retry-detached-owner",
            9,
            Some("trn_retry-detached-owner"),
            &Message::user("retry detached prompt"),
        )
        .await
        .unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "retry sub answer".into(),
    });
    session
        .submit_user_message_with_images_idempotent(
            "retry detached prompt".into(),
            Vec::new(),
            "retry-detached-owner".into(),
            9,
        )
        .await
        .unwrap();
    wait_until(|| {
        mock.requests().iter().any(|request| {
            request
                .messages
                .iter()
                .any(|message| message.text() == "retry detached prompt")
        })
    })
    .await;
    assert!(
        detached
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "retry detached prompt")
    );
    assert!(
        !fresh_main
            .history
            .lock()
            .unwrap()
            .rows()
            .iter()
            .any(|(_, message)| message.text() == "retry detached prompt")
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_recovery_does_not_redrive_an_already_answered_message() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let store = session.store.clone();
    core.close_session(&session_id).await;
    let message = Message::user("already answered");
    store
        .append_user_message_with_turn(
            &session_id,
            &AgentId::main(),
            0,
            "msg-answered",
            51,
            Some("trn_answered"),
            &message,
        )
        .await
        .unwrap();
    store
        .append_message(
            &session_id,
            &AgentId::main(),
            1,
            &Message::assistant_text("answer already persisted"),
        )
        .await
        .unwrap();

    core.open_session(Attach::Resume {
        session: session_id.clone(),
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(mock.requests().is_empty());
    assert_eq!(
        store
            .get_message_admission(&session_id, "msg-answered")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_user_message_id_survives_core_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("admission-restart.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    if let Some(session) = first_core.session(&session_id) {
        session
            .meta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .name = Some("restart test".into());
    }
    let mut events = first_core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Text("first answer".into()));
    let request = Request::UserMessage {
        text: "survive restart".into(),
        images: Vec::new(),
        message_id: Some("msg-restart".into()),
    };
    assert!(matches!(
        first_core.handle(&session_id, request.clone()).await,
        Reply::Ok
    ));
    assert_eq!(wait_main_turn_end(&mut events).await, TurnStop::Completed);
    assert_eq!(mock.requests().len(), 1);
    first_core.shutdown().await;

    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    if let Some(session) = second_core.session(&session_id) {
        session
            .meta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .zdr = true;
    }

    assert!(matches!(
        second_core.handle(&session_id, request.clone()).await,
        Reply::Ok
    ));
    let session = second_core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    assert_eq!(
        main.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(mock.requests().len(), 1);
    assert!(matches!(
        second_core
            .handle(
                &session_id,
                Request::UserMessage {
                    text: "different after restart".into(),
                    images: Vec::new(),
                    message_id: Some("msg-restart".into()),
                },
            )
            .await,
        Reply::Error { ref message } if message.contains("different content")
    ));
    second_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn acp_duplicate_replays_durable_turn_boundary_after_core_restart() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("acp-admission-restart.db");
    let mock = Arc::new(MockProvider::new("mock"));
    let first_core = build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    let session_id = first_core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    if let Some(session) = first_core.session(&session_id) {
        session
            .meta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .name = Some("ACP restart test".into());
    }
    let mut first_events = first_core.subscribe(&session_id).unwrap();
    let request = Request::UserMessage {
        text: "survive ACP restart".into(),
        images: Vec::new(),
        message_id: Some("acp-restart-fence".into()),
    };
    mock.push(MockResponse::Text("first answer".into()));
    assert!(matches!(
        first_core.handle(&session_id, request.clone()).await,
        Reply::Ok
    ));
    assert_eq!(
        wait_main_turn_end(&mut first_events).await,
        TurnStop::Completed
    );
    first_core.shutdown().await;

    let second_core =
        build_test_core(Store::open(&db_path).unwrap(), &temp_dir, mock.clone()).await;
    second_core
        .open_session(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    let mut events = second_core.subscribe(&session_id).unwrap();
    let restored_snapshot = second_core.snapshot(&session_id).unwrap();
    assert_eq!(
        restored_snapshot.terminal_turn,
        Some(TurnId::new("trn_acp-restart-fence"))
    );
    assert!(!restored_snapshot.turn_active);
    let restored_admission = second_core
        .session(&session_id)
        .unwrap()
        .store
        .get_message_admission(&session_id, "acp-restart-fence")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        restored_admission.state,
        pacode_store::MessageAdmissionState::Terminal
    );
    assert_eq!(
        restored_admission.turn_id,
        Some(TurnId::new("trn_acp-restart-fence"))
    );
    assert!(matches!(
        second_core.handle(&session_id, request).await,
        Reply::Ok
    ));
    let expected_turn = TurnId::new("trn_acp-restart-fence");
    let mut started = false;
    let mut ended = false;
    let timeout = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(timeout);
    while !started || !ended {
        tokio::select! {
            _ = &mut timeout => panic!("missing durable ACP turn boundary"),
            result = events.recv() => {
                if let Ok((_seq, event)) = result {
                    match event {
                        Event::TurnStarted { agent, turn }
                            if agent.is_main() && turn == expected_turn =>
                        {
                            started = true;
                        }
                        Event::TurnEnded { agent, turn, .. }
                            if agent.is_main() && turn == expected_turn =>
                        {
                            ended = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    assert_eq!(mock.requests().len(), 1);
    second_core.shutdown().await;
}

fn sub_spec(name: &str) -> pacode_tools::AgentSpec {
    pacode_tools::AgentSpec {
        name: Some(name.into()),
        prompt: "work".into(),
        tools: None,
        exclude_tools: None,
        fork: false,
        model: None,
        effort: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spawned_agent_inherits_parent_route() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let parent = session.main_agent().unwrap();
    let route = ModelRoute::new("mock", "parent-model");
    let effort = Effort::High;
    {
        let mut info = parent.info.write().unwrap();
        info.model = route.clone();
        info.effort = effort;
    }
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "child".into(),
    });

    let id = session.spawn_agent(sub_spec("child")).await.unwrap();
    let child = session.agent(&id).unwrap();
    assert_eq!(child.info().model, route);
    assert_eq!(child.info().effort, effort);
    assert_eq!(child.info().parent, Some(AgentId::main()));

    session.stop_agent(&id).await.unwrap();
    wait_until(|| !child.is_running()).await;
    core.shutdown().await;
}

/// `submit_user_message` returns after enqueueing, not when the turn ends —
/// otherwise `Request::Interrupt` could never arrive mid-turn (the ctrl+c
/// delay). Proven by interrupting a turn that is still streaming.
#[tokio::test(flavor = "multi_thread")]
async fn submit_returns_before_turn_end_and_interrupt_lands_mid_turn() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(400),
        text: "slow answer".into(),
    });
    let mut rx = core.subscribe(&session_id).unwrap();

    tokio::time::timeout(Duration::from_millis(200), async {
        session.submit_user_message("go".into()).await.unwrap();
    })
    .await
    .expect("submit must return long before the slow turn ends");

    let reply = core.handle(&session_id, Request::Interrupt).await;
    assert!(matches!(reply, Reply::Ok));

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for interrupted turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent.is_main()
                {
                    assert_eq!(stop, TurnStop::Interrupted);
                    break;
                }
            }
        }
    }
}

/// `InterruptAll` cancels the main agent's turn AND every live subagent, marking
/// them `Stopped` with history retained — and a winding-down subagent must not
/// re-drive the main turn.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_all_stops_main_and_subagents_without_restarting_main() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "subagent mid-work".into(),
    });
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "main mid-work".into(),
    });

    let sub_id = session.spawn_agent(sub_spec("sub")).await.unwrap();
    session.submit_user_message("go".into()).await.unwrap();

    // Let both turns actually start before the interrupt lands.
    let mut saw_main_start = false;
    let mut saw_sub_start = false;
    let start_timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(start_timeout);
    loop {
        tokio::select! {
            _ = &mut start_timeout => panic!("timed out waiting for both turns to start"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnStarted { agent, .. })) = res {
                    if agent.is_main() { saw_main_start = true; }
                    if agent == sub_id { saw_sub_start = true; }
                    if saw_main_start && saw_sub_start { break; }
                }
            }
        }
    }

    let reply = core.handle(&session_id, Request::InterruptAll).await;
    assert!(matches!(reply, Reply::Ok));

    let mut saw_main_end = false;
    let mut saw_sub_end = false;
    let end_timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(end_timeout);
    loop {
        tokio::select! {
            _ = &mut end_timeout => break,
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res {
                    if agent.is_main() { saw_main_end = true; }
                    if agent == sub_id { saw_sub_end = true; }
                    if saw_main_end && saw_sub_end { break; }
                }
            }
        }
    }
    assert!(saw_main_end && saw_sub_end, "both turns must end");

    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| !sub.is_running()).await;
    assert_eq!(sub.status(), AgentStatus::Stopped);
    assert!(
        !sub.history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .messages
            .is_empty(),
        "stopped agent keeps its history for `continue`"
    );
    let main = session.main_agent().unwrap();
    assert_eq!(main.status(), AgentStatus::Idle);

    // Give a hypothetical re-drive a window to happen: it must not.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !main.is_running(),
        "InterruptAll must not restart the main turn"
    );
    assert_eq!(main.status(), AgentStatus::Idle);
}

/// `continue` on a stopped subagent re-drives its turn on a fresh token: the
/// nudge lands in history, reaches the model as the last user message, and the
/// agent finishes normally.
#[tokio::test(flavor = "multi_thread")]
async fn continue_redrives_stopped_agent_with_nudge() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(200),
        text: "subagent mid-work".into(),
    });
    // The resumed turn's response, then the parent turn that the finished
    // subagent's report re-drives.
    mock.push(MockResponse::Text("resumed report".into()));
    mock.push(MockResponse::Text("noted".into()));

    let sub_id = session.spawn_agent(sub_spec("sub")).await.unwrap();
    session.stop_agent(&sub_id).await.unwrap();

    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| !sub.is_running()).await;
    assert_eq!(sub.status(), AgentStatus::Stopped);

    session.continue_agent(&sub_id).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for resumed turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent == sub_id
                {
                    break;
                }
            }
        }
    }

    wait_until(|| !sub.is_running()).await;
    assert_eq!(sub.status(), AgentStatus::Finished);

    // The stopped turn may have been cancelled before its provider call, so
    // locate the resumed request by its nudge rather than by index.
    let reqs = mock.requests();
    let resumed = reqs
        .iter()
        .find(|r| {
            r.messages
                .last()
                .is_some_and(|m| m.text().contains("[resumed by orchestrator"))
        })
        .expect("resumed request among recorded requests");
    let last = resumed.messages.last().expect("resumed request messages");
    assert_eq!(last.role, Role::User);
    assert!(
        last.text().contains("[resumed by orchestrator"),
        "resume nudge must be the last user message, got: {}",
        last.text()
    );

    // A fresh finish time: Stopped's stamp was cleared by the live status.
    assert!(sub.info().finished_at_ms.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_fences_a_waiting_continue_before_restart() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(200),
        text: "initial".into(),
    });
    let id = session.spawn_agent(sub_spec("fenced")).await.unwrap();
    let agent = session.agent(&id).unwrap();
    session.stop_agent(&id).await.unwrap();
    wait_until(|| !agent.is_running()).await;
    let requests_before = mock.requests().len();
    let generation = agent.turn_generation();
    let message_guard = agent.message_write_lock.clone().lock_owned().await;

    let continue_task = tokio::spawn({
        let session = session.clone();
        let id = id.clone();
        async move { session.continue_agent(&id).await }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if session.submit_lock.try_lock().is_err() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "continue did not enter the lifecycle fence"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;

    let stop_task = tokio::spawn({
        let session = session.clone();
        let id = id.clone();
        async move { session.stop_agent(&id).await }
    });
    wait_until(|| agent.turn_generation() != generation).await;
    drop(message_guard);

    assert!(continue_task.await.unwrap().is_err());
    stop_task.await.unwrap().unwrap();
    assert!(!agent.is_running());
    assert!(agent.stop_requested());
    assert_eq!(mock.requests().len(), requests_before);
    core.shutdown().await;
}

/// `continue` refuses agents that are not stopped: unknown id, still running,
/// or already finished.
#[tokio::test(flavor = "multi_thread")]
async fn continue_errors_on_live_or_unknown_agent() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    let err = session
        .continue_agent(&AgentId::new("agt_missing"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("agent not found"), "got: {err}");

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "working".into(),
    });
    let running_id = session.spawn_agent(sub_spec("running")).await.unwrap();
    let err = session.continue_agent(&running_id).await.unwrap_err();
    assert!(
        err.to_string().contains("still running"),
        "running agent must not be resumable, got: {err}"
    );
    session.stop_agent(&running_id).await.unwrap();

    mock.push(MockResponse::Text("quick done".into()));
    mock.push(MockResponse::Text("main auto".into()));
    let done_id = session.spawn_agent(sub_spec("done")).await.unwrap();
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for sub turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent == done_id
                {
                    break;
                }
            }
        }
    }
    let done = session.agent(&done_id).unwrap();
    wait_until(|| !done.is_running()).await;
    assert_eq!(done.status(), AgentStatus::Finished);
    let err = session.continue_agent(&done_id).await.unwrap_err();
    assert!(
        err.to_string().contains("not stopped"),
        "finished agent is not resumable, got: {err}"
    );
}

/// Resuming keeps only the transcript tail, and the item counter continues past
/// it: a new item must not land on the seq of a restored one.
#[test]
fn restored_transcript_items_do_not_collide_with_new_ones() {
    use pacode_types::{AgentId, Message};
    use std::sync::Arc;

    let messages: Vec<(u64, Arc<Message>)> = (0..10)
        .map(|i| (i, Arc::new(Message::user(format!("message {i}")))))
        .collect();

    let tail_cap = 4usize;
    let tail_start = messages.len().saturating_sub(tail_cap);
    let entries = crate::transcript::history_to_items(&AgentId::main(), &messages[tail_start..]);
    assert_eq!(entries.len(), tail_cap, "only the tail is rendered");
    assert_eq!(entries[0].message_seq, tail_start as u64);

    // The tail is numbered by the transcript's own counter, lifted above the
    // band reserved for store history.
    let mut state = crate::transcript::TranscriptState::new(tail_cap);
    state.reserve_history_band();
    let first = state.next_seq();
    for entry in entries {
        let mut item = entry.item;
        item.seq = state.next_seq();
        state.upsert(item);
    }

    let next = state.next_seq();
    assert!(next > first, "seqs must advance past the restored tail");
    assert!(
        state.tail.iter().all(|item| item.seq < next),
        "a new item must not reuse a restored seq"
    );
    assert!(
        state
            .tail
            .iter()
            .all(|item| item.seq >= crate::transcript::HISTORY_ITEM_BASE),
        "tail items must sit above the store band so pages cannot collide"
    );
}

/// Ctrl+C while a permission prompt is still pending must resolve it like a
/// denial: the entry leaves the pending map (a later live request is not
/// shadowed by a stale one), the waiter resolves to `Deny`, the transcript
/// cell flips to the outcome notice, and `PermissionResolved` carries the
/// `interrupted` reason.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_denies_pending_permission() {
    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    let main = session.main_agent().unwrap();
    let req = pacode_types::PermissionRequest {
        id: pacode_types::PermissionId::generate(),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: pacode_types::CallId::new("call_test"),
        tool: "bash".into(),
        title: "Bash: rm -rf ~".into(),
        detail: "rm -rf ~".into(),
        risk: None,
        created_at_ms: pacode_types::now_ms(),
    };
    let waiter = session
        .permissions
        .register(req.clone())
        .expect("permission should register");
    let seq = main
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .next_seq();
    main.transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(pacode_types::TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: req.created_at_ms,
            kind: pacode_types::TranscriptKind::Permission(req.clone()),
        });

    session.interrupt();

    // The pending map is clean and the parked waiter is released.
    assert!(session.permissions.pending().is_empty());
    let (decision, reason) = waiter.await.expect("cancelled permission resolves");
    assert_eq!(decision, pacode_types::PermissionDecision::Deny);
    assert_eq!(
        reason.as_deref(),
        Some(crate::permissions::CANCELLED_BY_AGENT_REASON)
    );

    // The transcript Permission cell flipped to the interrupted-denial notice.
    {
        let transcript = main.transcript.lock().unwrap_or_else(|p| p.into_inner());
        let item = transcript
            .tail
            .iter()
            .find(|i| i.seq == seq)
            .expect("permission cell must still be in the tail");
        match &item.kind {
            pacode_types::TranscriptKind::Notice { text, .. } => {
                assert_eq!(text, "Permission denied: interrupted");
            }
            other => panic!("permission cell must become a notice, got {other:?}"),
        }
    }

    // The wire event says Deny + why.
    let timeout = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("no PermissionResolved emitted"),
            res = rx.recv() => {
                if let Ok((_seq, Event::PermissionResolved { permission, decision, reason })) = res {
                    assert_eq!(permission, req.id);
                    assert_eq!(decision, pacode_types::PermissionDecision::Deny);
                    assert_eq!(reason.as_deref(), Some("interrupted"));
                    break;
                }
            }
        }
    }
    let mut resolved_events = 1;
    while let Ok((_seq, event)) = rx.try_recv() {
        if matches!(event, Event::PermissionResolved { permission, .. } if permission == req.id) {
            resolved_events += 1;
        }
    }
    assert_eq!(resolved_events, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_all_cancels_pending_question() {
    use pacode_types::question::{Question, QuestionOption, QuestionOrigin};

    let (core, _mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let question = Question::new(
        pacode_types::QuestionId::generate(),
        QuestionOrigin::new(AgentId::main(), "main", pacode_types::CallId::generate()),
        "Choice",
        "Continue?",
        vec![
            QuestionOption::new("yes", ""),
            QuestionOption::new("no", ""),
        ],
        false,
        pacode_types::now_ms(),
    )
    .unwrap();
    let waiter = session.questions.register(question).unwrap();

    assert!(matches!(
        core.handle(&session_id, Request::InterruptAll).await,
        Reply::Ok
    ));
    assert!(session.questions.pending().is_empty());
    assert!(waiter.await.unwrap().cancelled);
}

/// A prompt sent while Ctrl+C unwinds the turn lands in `agent.injections`
/// (the agent still reads as running). The dying turn never drains it, so it
/// used to be orphaned until the next user message: the turn must re-drive
/// once, draining the queued steer.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_redrives_queued_steer() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    // Turn 1 streams slowly; while it runs we queue a steer and interrupt.
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(400),
        text: "first answer".into(),
    });
    session.submit_user_message("first".into()).await.unwrap();

    // Give the turn a moment to enter the stream, then queue the steer (the
    // agent is still running, so this becomes an Injection::UserSteer) and
    // interrupt.
    tokio::time::sleep(Duration::from_millis(50)).await;
    session.submit_user_message("second".into()).await.unwrap();
    let reply = core.handle(&session_id, Request::Interrupt).await;
    assert!(matches!(reply, Reply::Ok));

    // The re-driven turn's provider call.
    mock.push(MockResponse::Text("after steer".into()));

    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    let mut ends: Vec<TurnStop> = Vec::new();
    loop {
        tokio::select! {
            _ = &mut timeout => break,
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent.is_main()
                {
                    ends.push(stop);
                    if ends.len() >= 2 { break; }
                }
            }
        }
    }
    assert_eq!(
        ends,
        vec![TurnStop::Interrupted, TurnStop::Completed],
        "the queued steer must re-drive the turn after the interrupt"
    );

    let recorded_requests = mock.requests();
    let steer_requests = recorded_requests
        .iter()
        .filter(|request| {
            request
                .messages
                .iter()
                .any(|message| message.role == Role::User && message.text() == "second")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        steer_requests.len(),
        1,
        "queued steer must reach the provider once"
    );
    assert!(
        steer_requests[0]
            .messages
            .iter()
            .all(|message| !message.text().contains("<user_message>"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_redrives_two_correlated_steers_in_order() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut events = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(300),
        text: "first answer".into(),
    });
    session
        .submit_user_message_with_images_idempotent(
            "first".into(),
            Vec::new(),
            "msg-two-steer-1".into(),
            1,
        )
        .await
        .unwrap();
    wait_until(|| !mock.requests().is_empty()).await;
    session
        .submit_user_message_with_images_idempotent(
            "second".into(),
            Vec::new(),
            "msg-two-steer-2".into(),
            2,
        )
        .await
        .unwrap();
    session
        .submit_user_message_with_images_idempotent(
            "third".into(),
            Vec::new(),
            "msg-two-steer-3".into(),
            3,
        )
        .await
        .unwrap();
    mock.push(MockResponse::Text("after second steer".into()));
    mock.push(MockResponse::Text("after third steer".into()));
    assert!(matches!(
        core.handle(&session_id, Request::Interrupt).await,
        Reply::Ok
    ));

    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    let mut ends = Vec::new();
    while ends.len() < 3 {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for batched steer turn"),
            result = events.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = result
                    && agent.is_main()
                {
                    ends.push(stop);
                }
            }
        }
    }
    assert_eq!(
        ends,
        vec![
            TurnStop::Interrupted,
            TurnStop::Completed,
            TurnStop::Completed
        ]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| message.role == Role::User && message.text() == "second")
    }));
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| message.role == Role::User && message.text() == "third")
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_all_discards_admitted_queued_steer() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut events = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(400),
        text: "first answer".into(),
    });

    session.submit_user_message("first".into()).await.unwrap();
    wait_until(|| !mock.requests().is_empty()).await;
    session
        .submit_user_message_with_images_idempotent(
            "cancel me".into(),
            Vec::new(),
            "msg-cancel".into(),
            1,
        )
        .await
        .unwrap();
    assert!(session.main_agent().unwrap().has_pending_user_messages());

    assert!(matches!(
        core.handle(&session_id, Request::InterruptAll).await,
        Reply::Ok
    ));
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for interrupted turn"),
            result = events.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = result
                    && agent.is_main()
                {
                    assert_eq!(stop, TurnStop::Interrupted);
                    break;
                }
            }
        }
    }

    let main = session.main_agent().unwrap();
    assert!(!main.is_running());
    assert!(!main.has_pending_user_messages());
    assert!(main.injections.is_empty());
    assert_eq!(
        session
            .store
            .get_message_admission(&session_id, "msg-cancel")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_racing_with_post_block_redrive_starts_the_claimed_turn() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::Text("raced answer".into()));

    let post_block_guard = main.turn_lock.clone().try_lock_owned().unwrap();
    let submit_session = session.clone();
    let submit = tokio::spawn(async move {
        submit_session
            .submit_user_message("raced prompt".into())
            .await
            .unwrap();
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while main.injections.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(!main.injections.is_empty());
    assert!(main.has_pending_user_messages());
    assert!(
        !main
            .history
            .lock()
            .unwrap()
            .messages
            .iter()
            .any(|message| message.text() == "raced prompt")
    );
    drop(post_block_guard);
    crate::turn::lifecycle::request_turn(session.clone(), main.clone());

    submit.await.unwrap();
    assert_eq!(wait_main_turn_end(&mut rx).await, TurnStop::Completed);
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .filter(|message| message.role == Role::User && message.text() == "raced prompt")
            .count(),
        1
    );
    assert!(
        requests[0]
            .messages
            .iter()
            .all(|message| !message.text().contains("<user_message>"))
    );
    assert!(!main.has_pending_user_messages());
}

/// A provider failure on the main agent ends the turn `Failed` and lands the
/// agent back at `Idle` — before the fix the status stayed live (`Thinking`)
/// forever even though no turn was running.
#[tokio::test(flavor = "multi_thread")]
async fn failed_turn_on_main_lands_idle() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::Error("provider blew up".into()));
    session.submit_user_message("go".into()).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for failed turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent.is_main()
                {
                    match stop {
                        TurnStop::Failed { message } => {
                            assert!(message.contains("provider blew up"), "got: {message}");
                        }
                        other => panic!("expected Failed, got {other:?}"),
                    }
                    break;
                }
            }
        }
    }

    let main = session.main_agent().unwrap();
    wait_until(|| !main.is_running()).await;
    assert_eq!(main.status(), AgentStatus::Idle);
}

/// The same failure on a subagent must stamp `Failed` with the error recorded
/// (not a fake `Finished` ✓) and still notify the parent — the orchestrator
/// may be parked waiting for the report.
#[tokio::test(flavor = "multi_thread")]
async fn failed_turn_on_subagent_lands_failed_and_reports_parent() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    // The subagent's provider call fails; the parent's re-driven turn gets a
    // plain reply.
    mock.push(MockResponse::Error("provider blew up".into()));
    mock.push(MockResponse::Text("noted".into()));

    let sub_id = session.spawn_agent(sub_spec("sub")).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for subagent turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent == sub_id
                {
                    match stop {
                        TurnStop::Failed { message } => {
                            assert!(message.contains("provider blew up"), "got: {message}");
                        }
                        other => panic!("expected Failed, got {other:?}"),
                    }
                    break;
                }
            }
        }
    }

    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| !sub.is_running() && sub.status() == AgentStatus::Failed).await;
    let info = sub.info();
    assert_eq!(info.status, AgentStatus::Failed);
    assert!(
        info.error
            .as_deref()
            .is_some_and(|e| e.contains("provider blew up")),
        "subagent error must be recorded, got: {:?}",
        info.error
    );
    assert!(
        info.summary.is_none(),
        "a failed subagent must not get a fake summary"
    );

    // The parent is re-driven with the failure report.
    wait_until(|| {
        mock.requests().iter().any(|r| {
            r.messages.iter().any(|m| {
                m.text().contains("<agent_finished") && m.text().contains("subagent failed")
            })
        })
    })
    .await;
}

/// A panic inside the turn task (here: a tool that panics) is supervised:
/// the cancel token is cleared, the main agent lands `Failed`, and a
/// `TurnEnded` carrying the same `turn_id` as `TurnStarted` is still emitted.
#[tokio::test(flavor = "multi_thread")]
async fn panicking_tool_turn_on_main_is_supervised() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("panic_tool".into(), serde_json::json!({}))],
    });
    session.submit_user_message("go".into()).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    let mut started_turn: Option<TurnId> = None;
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for crashed turn end"),
            res = rx.recv() => {
                match res {
                    Ok((_seq, Event::TurnStarted { agent, turn })) if agent.is_main() => {
                        started_turn = Some(turn);
                    }
                    Ok((_seq, Event::TurnEnded { agent, turn, stop, .. })) if agent.is_main() => {
                        match stop {
                            TurnStop::Failed { message } => {
                                assert!(message.contains("panicked"), "got: {message}");
                            }
                            other => panic!("expected Failed, got {other:?}"),
                        }
                        assert_eq!(
                            Some(turn),
                            started_turn,
                            "the synthesized TurnEnded must carry the started turn's id"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let main = session.main_agent().unwrap();
    wait_until(|| !main.is_running()).await;
    assert_eq!(main.status(), AgentStatus::Failed);
    assert!(
        main.info()
            .error
            .as_deref()
            .is_some_and(|e| e.contains("panicked")),
        "panic must be recorded on the agent info"
    );
    let totals = session.store.usage_totals(&session.id).await.unwrap();
    assert_eq!(totals.input, 100);
    assert_eq!(totals.output, 20);

    // The crashed turn's `tool_use` must not stay dangling in persisted
    // history: the supervisor writes a stub result for it.
    let panic_call_id = {
        let hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
        hist.messages
            .iter()
            .flat_map(|m| m.tool_uses())
            .find(|(_, name, _)| *name == "panic_tool")
            .map(|(id, _, _)| id.clone())
            .expect("panic_tool tool_use persisted")
    };
    {
        let hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
        let stubbed = hist.messages.iter().any(|m| {
            m.role == Role::Tool
                && m.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolResult { call_id, .. } if *call_id == panic_call_id)
                })
        });
        assert!(
            stubbed,
            "crash must persist a stub tool_result for the call"
        );
    }

    // The next turn's request carries the pair cleanly — the exact 400 the
    // orphaned `tool_use` used to produce on every provider.
    mock.push(MockResponse::Text("ok".into()));
    session.submit_user_message("next".into()).await.unwrap();
    wait_main_turn_end(&mut rx).await;
    let req = mock.requests().last().unwrap().clone();
    assert_no_orphan_tool_uses(&req.messages);
}

/// A panicking subagent turn: the supervisor clears the token, the post-block
/// stamps `Failed` (not `Finished`) and the parent still gets its report.
#[tokio::test(flavor = "multi_thread")]
async fn panicking_tool_turn_on_subagent_is_supervised_and_reports_parent() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("panic_tool".into(), serde_json::json!({}))],
    });
    // The parent's re-driven turn after the crash report lands.
    mock.push(MockResponse::Text("acknowledged".into()));

    let sub_id = session.spawn_agent(sub_spec("sub")).await.unwrap();

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for crashed subagent turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, stop, .. })) = res
                    && agent == sub_id
                {
                    match stop {
                        TurnStop::Failed { message } => {
                            assert!(message.contains("panicked"), "got: {message}");
                        }
                        other => panic!("expected Failed, got {other:?}"),
                    }
                    break;
                }
            }
        }
    }

    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| !sub.is_running() && sub.status() == AgentStatus::Failed).await;
    assert!(
        sub.info()
            .error
            .as_deref()
            .is_some_and(|e| e.contains("panicked")),
        "panic must be recorded on the subagent info"
    );

    // The parent's next request must carry the failure report — the
    // orchestrator would otherwise wait on the subagent forever.
    wait_until(|| {
        mock.requests().iter().any(|r| {
            r.messages.iter().any(|m| {
                let t = m.text();
                t.contains("<agent_finished")
                    && t.contains("subagent failed")
                    && t.contains("panicked")
            })
        })
    })
    .await;
}

/// `handle_interrupted` mapping: the main agent rests at `Idle`, but a
/// subagent has no resting live state — interrupted without a stop request it
/// must land `Stopped` (terminal, resumable) instead of a permanent fake
/// "live" `Idle`.
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_subagent_without_stop_request_lands_stopped() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Let the spawned turn end fast (provider error → Failed post-block) so
    // the direct `handle_interrupted` call exercises the mapping cleanly.
    mock.push(MockResponse::Error("sub turn over".into()));
    mock.push(MockResponse::Text("parent noted".into()));

    let sub_id = session.spawn_agent(sub_spec("sub")).await.unwrap();
    let sub = session.agent(&sub_id).unwrap();
    wait_until(|| !sub.is_running() && sub.status() == AgentStatus::Failed).await;

    let stop = crate::turn::lifecycle::handle_interrupted(&session, &sub);
    assert_eq!(stop, TurnStop::Interrupted);
    assert_eq!(
        sub.status(),
        AgentStatus::Stopped,
        "a subagent interrupted without a stop request must not sit at live `Idle`"
    );

    // With the stop flag raised it stays `Stopped` too (the pre-existing path).
    sub.request_stop();
    let stop = crate::turn::lifecycle::handle_interrupted(&session, &sub);
    assert_eq!(stop, TurnStop::Interrupted);
    assert_eq!(sub.status(), AgentStatus::Stopped);

    let main = session.main_agent().unwrap();
    let stop = crate::turn::lifecycle::handle_interrupted(&session, &main);
    assert_eq!(stop, TurnStop::Interrupted);
    assert_eq!(main.status(), AgentStatus::Idle);
}

/// A persisted `tool_use` that lost its `tool_result` (a crash under an older
/// build, a lost store write) self-heals at request build: `for_model` injects
/// a synthetic result so the provider never sees the orphan.
#[tokio::test(flavor = "multi_thread")]
async fn orphan_tool_use_in_history_is_repaired_in_next_request() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    // Simulate a corrupted persisted tail: an assistant `tool_use` with no
    // `tool_result` behind it.
    main.history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: CallId::new("call_orphaned"),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
        ));

    mock.push(MockResponse::Text("ok".into()));
    session.submit_user_message("hi".into()).await.unwrap();
    wait_main_turn_end(&mut rx).await;

    let req = mock.requests().last().unwrap().clone();
    let pos = req
        .messages
        .iter()
        .position(|m| {
            m.tool_uses()
                .any(|(id, _, _)| id.as_str() == "call_orphaned")
        })
        .expect("orphan tool_use must still be in the request");
    let next = &req.messages[pos + 1];
    assert_eq!(
        next.role,
        Role::Tool,
        "a synthetic result must directly follow the orphaned tool_use"
    );
    assert!(next.content.iter().any(|b| matches!(
        b,
        ContentBlock::ToolResult {
            call_id,
            is_error: true,
            ..
        } if call_id.as_str() == "call_orphaned"
    )));
    assert_no_orphan_tool_uses(&req.messages);
}

/// Interrupt while a tool is mid-execution: the in-flight call still gets a
/// persisted result ("cancelled"), so the next request pairs cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_mid_tool_call_leaves_no_orphan() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let mut rx = core.subscribe(&session_id).unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("block_tool".into(), serde_json::json!({}))],
    });
    session.submit_user_message("go".into()).await.unwrap();

    // Interrupt only once the tool is actually running, then wait out the
    // unwind: the interrupted turn lands the agent at `Idle`.
    wait_until(|| main.status() == AgentStatus::RunningTool).await;
    session.interrupt();
    assert_eq!(wait_main_turn_end(&mut rx).await, TurnStop::Interrupted);
    wait_until(|| !main.is_running()).await;

    // The persisted tail must answer the dispatched call.
    let block_call_id = {
        let hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
        hist.messages
            .iter()
            .flat_map(|m| m.tool_uses())
            .find(|(_, name, _)| *name == "block_tool")
            .map(|(id, _, _)| id.clone())
            .expect("block_tool tool_use persisted")
    };
    {
        let hist = main.history.lock().unwrap_or_else(|p| p.into_inner());
        let stubbed = hist.messages.iter().any(|m| {
            m.role == Role::Tool
                && m.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolResult { call_id, .. } if *call_id == block_call_id)
                })
        });
        assert!(stubbed, "interrupt must persist a tool_result for the call");
    }

    // And the next request is well formed.
    mock.push(MockResponse::Text("ok".into()));
    session.submit_user_message("next".into()).await.unwrap();
    wait_main_turn_end(&mut rx).await;
    let req = mock.requests().last().unwrap().clone();
    assert_no_orphan_tool_uses(&req.messages);
}

#[test]
fn optimistic_snapshot_retries_when_event_seq_changes() {
    let events = crate::transcript::EventSink::new();
    let mut attempts = 0;
    let mut boundary = None;
    let state = crate::session::optimistic_snapshot(&events, |seq| {
        boundary = Some(seq);
        attempts += 1;
        if attempts == 1 {
            events.emit(Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "race".into(),
                detail: None,
            });
        }
        attempts
    });

    assert_eq!(boundary, Some(1));
    assert_eq!(state, 2);
    assert_eq!(attempts, 2);
}

/// What a subagent's tool set ends up being, so the selection rules can be
/// asserted without reaching through the registry.
async fn spawned_tool_names(session: &std::sync::Arc<Session>, id: &AgentId) -> Vec<String> {
    let agent = session.agent(id).expect("spawned agent is in the session");
    agent
        .tools
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .names()
        .map(str::to_string)
        .collect()
}

fn spec(tools: Option<&[&str]>, exclude_tools: Option<&[&str]>) -> pacode_tools::AgentSpec {
    pacode_tools::AgentSpec {
        prompt: "work".to_string(),
        name: None,
        model: None,
        effort: None,
        fork: false,
        tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        exclude_tools: exclude_tools.map(|t| t.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
    }
}

/// The default has to be parity: a subagent that needs a list to become useful
/// is a subagent the orchestrator has to think about on every spawn, which is
/// exactly the friction that made fan-out feel expensive.
#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_starts_with_the_orchestrators_own_tool_set() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let parent_tools: Vec<String> = session
        .tools
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .names()
        .map(str::to_string)
        .collect();
    mock.push(MockResponse::Slow {
        delay: std::time::Duration::from_millis(100),
        text: "done".into(),
    });

    let id = session.spawn_agent(spec(None, None)).await.unwrap();
    let child = spawned_tool_names(&session, &id).await;

    assert_eq!(
        child, parent_tools,
        "omitting both lists must hand the subagent the same set the orchestrator has"
    );
}

/// `exclude_tools` exists so trimming costs a short list instead of restating
/// the whole one — at 128 subagents every schema is re-sent on every step.
#[tokio::test(flavor = "multi_thread")]
async fn exclude_tools_drops_those_tools_and_keeps_the_rest() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    let parent_tools: Vec<String> = session
        .tools
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .names()
        .map(str::to_string)
        .collect();
    let dropped = parent_tools
        .first()
        .cloned()
        .expect("the harness registers at least one tool");
    mock.push(MockResponse::Slow {
        delay: std::time::Duration::from_millis(100),
        text: "done".into(),
    });

    let id = session
        .spawn_agent(spec(None, Some(&[dropped.as_str()])))
        .await
        .unwrap();
    let child = spawned_tool_names(&session, &id).await;

    assert!(
        !child.contains(&dropped),
        "{dropped} should be gone: {child:?}"
    );
    assert_eq!(
        child.len(),
        parent_tools.len() - 1,
        "exactly one tool should have been dropped: {child:?}"
    );
}

/// Both lists at once: the include decides the set, the exclude subtracts from
/// it. Exclude wins on a name that appears in both, so a preset can refine a
/// wider set without having to know what it does not contain.
#[tokio::test(flavor = "multi_thread")]
async fn include_and_exclude_compose_with_exclude_winning() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: std::time::Duration::from_millis(100),
        text: "done".into(),
    });

    // The harness registers exactly these two, so they are what the include
    // list is built from — naming a tool the session does not have is ignored,
    // and a test that invented names would pass on an empty set.
    let id = session
        .spawn_agent(spec(
            Some(&["panic_tool", "block_tool"]),
            Some(&["block_tool"]),
        ))
        .await
        .unwrap();
    let child = spawned_tool_names(&session, &id).await;

    assert_eq!(
        child,
        vec!["panic_tool".to_string()],
        "include picks the set and exclude subtracts from it, winning on overlap"
    );
}

/// Depth 1 is structural. No combination of arguments may hand a subagent the
/// means to spawn one, because a fleet that fans out recursively has no bound
/// the orchestrator can reason about.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_tool_cannot_be_granted_to_a_subagent() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();
    mock.push(MockResponse::Slow {
        delay: std::time::Duration::from_millis(100),
        text: "done".into(),
    });

    let id = session
        .spawn_agent(spec(Some(&["ls", "agent"]), None))
        .await
        .unwrap();
    let child = spawned_tool_names(&session, &id).await;

    assert!(
        !child.contains(&"agent".to_string()),
        "a subagent must never be able to spawn one: {child:?}"
    );
}
