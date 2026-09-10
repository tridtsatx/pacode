use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentStatus, Attach, Config, Event, ModelRoute, Reply, Request, Role, TurnStop,
};

use crate::core::{Core, CoreDeps};

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
    config.agents.max_live = 2;

    let paths = Paths::under(temp_dir.path());
    let tools = ToolRegistry::new();

    let deps = CoreDeps {
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
async fn test_detach_at_live_subagent_cap_fails() {
    let (core, mock, _tmp, session_id) = setup_test_session().await;
    let session = core.session(&session_id).unwrap();

    // Spawn 2 subagents to reach config.agents.max_live (set to 2 in setup)
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

    // Detach main turn must fail because live_count (2) >= max_live (2)
    let err = session.detach_main_turn().await.unwrap_err();
    assert!(
        err.to_string().contains("maximum live subagents"),
        "expected maximum live subagents error, got: {err}"
    );
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
