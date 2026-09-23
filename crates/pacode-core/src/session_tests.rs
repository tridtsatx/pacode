use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, ToolRegistry};
use pacode_types::{
    AgentId, AgentStatus, Attach, CallId, Config, ContentBlock, Effort, Event, Message, ModelRoute,
    Reply, Request, Role, TurnId, TurnStop,
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

async fn setup_test_session() -> (
    Arc<Core>,
    Arc<MockProvider>,
    tempfile::TempDir,
    pacode_types::SessionId,
) {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();

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

    let mock_provider = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock_provider.clone());
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

    let core = Core::new(deps).await;
    let cwd = temp_dir.path().to_path_buf();
    let session_id = core
        .open_session(Attach::New {
            cwd,
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

    // Wait until turn starts and first delta arrives
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for initial text delta"),
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
        tokio::select! {
            _ = &mut timeout => break,
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
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for Turn 1 to complete"),
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
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn 2 delta"),
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
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for Turn 3 on fresh main"),
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

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for stream start"),
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
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for detached agent to finish"),
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

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    // Wait until first delta arrives (mid-stream)
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for first delta"),
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
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn completion"),
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

fn sub_spec(name: &str) -> pacode_tools::AgentSpec {
    pacode_tools::AgentSpec {
        name: Some(name.into()),
        prompt: "work".into(),
        tools: None,
        fork: false,
        model: None,
        effort: None,
    }
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
    let items = crate::transcript::history_to_items(
        &AgentId::main(),
        &messages[tail_start..],
        tail_start as u64,
    );
    assert_eq!(items.len(), tail_cap, "only the tail is rendered");
    assert_eq!(items[0].seq, tail_start as u64);

    let mut state = crate::transcript::TranscriptState::new(tail_cap);
    let mut highest = None;
    for item in items {
        highest = Some(item.seq);
        state.upsert(item);
    }
    state.next_item_seq = highest.expect("items") + 1;

    let next = state.next_seq();
    assert_eq!(next, messages.len() as u64);
    assert!(
        state.tail.iter().all(|item| item.seq < next),
        "a new item must not reuse a restored seq"
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
    let waiter = session.permissions.register(req.clone());
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
    assert!(
        waiter.await.is_err(),
        "dropped sender resolves the waiter to Deny"
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

    // The steer reached the model: some request carries the injected message.
    let saw_steer = mock.requests().iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.text().contains("<user_message>second</user_message>"))
    });
    assert!(saw_steer, "queued steer must reach the provider");
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
        assert!(stubbed, "crash must persist a stub tool_result for the call");
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
        .position(|m| m.tool_uses().any(|(id, _, _)| id.as_str() == "call_orphaned"))
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
