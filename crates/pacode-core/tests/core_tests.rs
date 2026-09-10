use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_core::core::{Core, CoreDeps};
use pacode_exec::{TaskManager, TaskSpec};
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::host::PermissionDraft;
use pacode_tools::{AgentSpec, Tool, ToolCtx, ToolError, ToolKind, ToolOutput, ToolRegistry};
use pacode_types::{
    Attach, Config, ContentBlock, Event, Mode, ModelRoute, PermissionDecision, Request, RiskLevel,
    Role, ToolStatus, TranscriptKind, TurnStop,
};

struct StubBash;
#[async_trait::async_trait]
impl Tool for StubBash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "Execute bash commands"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "background": {"type": "boolean"}
            },
            "required": ["command"]
        })
    }
    async fn call(&self, input: serde_json::Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let cmd = input
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("echo hi");
        let risk = if cmd.contains("rm -rf") {
            RiskLevel::Catastrophic
        } else {
            RiskLevel::Confirm
        };
        let decision = ctx
            .host
            .request_permission(PermissionDraft {
                agent: ctx.agent.clone(),
                agent_name: ctx.agent_name.clone(),
                call_id: ctx.call_id.clone(),
                title: format!("Bash: {cmd}"),
                detail: cmd.to_string(),
                risk: Some(risk),
                tool_name: Some("bash".to_string()),
                tool_kind: Some(ToolKind::Exec),
            })
            .await;
        match decision {
            PermissionDecision::Deny => Err(ToolError::Denied("user denied execution".to_string())),
            _ => Ok(ToolOutput::text("hi\n")),
        }
    }
}

struct StubWrite;
#[async_trait::async_trait]
impl Tool for StubWrite {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Write file"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        })
    }
    async fn call(&self, input: serde_json::Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let path = input
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("test.txt");
        let content = input.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let decision = ctx
            .host
            .request_permission(PermissionDraft {
                agent: ctx.agent.clone(),
                agent_name: ctx.agent_name.clone(),
                call_id: ctx.call_id.clone(),
                title: format!("Write {path}"),
                detail: format!("{path}\n{content}"),
                risk: None,
                tool_name: Some("write".to_string()),
                tool_kind: Some(ToolKind::Edit),
            })
            .await;
        match decision {
            PermissionDecision::Deny => Err(ToolError::Denied("user denied write".to_string())),
            _ => {
                let file_path = ctx.cwd.join(path);
                tokio::fs::write(&file_path, content)
                    .await
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
                Ok(ToolOutput::text(format!(
                    "Wrote {} bytes to {}",
                    content.len(),
                    path
                )))
            }
        }
    }
}

struct StubAgent;
#[async_trait::async_trait]
impl Tool for StubAgent {
    fn name(&self) -> &'static str {
        "agent"
    }
    fn description(&self) -> &'static str {
        "Spawn subagent"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "name": {"type": "string"}
            },
            "required": ["prompt"]
        })
    }
    async fn call(&self, input: serde_json::Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let prompt = input.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
        let name = input.get("name").and_then(|n| n.as_str()).map(String::from);
        let id = ctx
            .host
            .spawn_agent(AgentSpec {
                name,
                prompt: prompt.to_string(),
                tools: None,
                fork: false,
                model: None,
                effort: None,
            })
            .await?;
        Ok(ToolOutput::text(format!("Agent spawned: {id}")))
    }
}

struct StubTriggerBgTask;
#[async_trait::async_trait]
impl Tool for StubTriggerBgTask {
    fn name(&self) -> &'static str {
        "trigger_bg"
    }
    fn description(&self) -> &'static str {
        "Trigger background task"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let mut spec = TaskSpec::new(
            ctx.session.clone(),
            ctx.agent.clone(),
            "echo task-finished-42",
            ctx.cwd.clone(),
        );
        spec.background = true;
        spec.label = Some("bg-calc".into());
        let task_id = ctx.host.spawn_task(spec).await?;
        let _ = ctx
            .host
            .wait_task(&task_id, Duration::from_secs(2), false)
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(ToolOutput::text("triggered"))
    }
}

async fn setup_test_core() -> (Arc<Core>, Arc<MockProvider>, tempfile::TempDir) {
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
    config.context.keep_recent_messages = 4;

    let paths = Paths::under(temp_dir.path());

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(StubBash));
    tools.register(Arc::new(StubWrite));
    tools.register(Arc::new(StubAgent));
    tools.register(Arc::new(StubTriggerBgTask));

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
    (core, mock_provider, temp_dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_01_text_only_turn() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

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

    mock.push(MockResponse::Text("Hello, user!".into()));

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "Hello assistant".into(),
        },
    )
    .await;

    let mut saw_delta = false;
    let saw_turn_ended;

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for events"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                match event {
                    Event::TextDelta { text, .. } => {
                        if text.contains("Hello") {
                            saw_delta = true;
                        }
                    }
                    Event::TurnEnded { stop, .. } => {
                        assert_eq!(stop, TurnStop::Completed);
                        saw_turn_ended = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(saw_delta);
    assert!(saw_turn_ended);

    let session = core.session(&session_id).unwrap();
    let main_agent = session.main_agent().unwrap();
    let messages = main_agent.history.lock().unwrap().messages.clone();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[0].text(), "Hello assistant");
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[1].text(), "Hello, user!");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_02_tool_turn_bypass_mode() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

    let session_id = core
        .open_session(Attach::New {
            cwd,
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: Some(Mode::Bypass),
        })
        .await
        .unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("bash".into(), serde_json::json!({"command": "echo hi"}))],
    });
    mock.push(MockResponse::Text("Command completed".into()));

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "run bash".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    let mut saw_tool_ok = false;

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for events"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                match event {
                    Event::ItemUpdated(item) => {
                        if let TranscriptKind::ToolCall { status, .. } = item.kind
                            && status == ToolStatus::Ok
                        {
                            saw_tool_ok = true;
                        }
                    }
                    Event::TurnEnded { .. } => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(saw_tool_ok);

    let session = core.session(&session_id).unwrap();
    let main_agent = session.main_agent().unwrap();
    let messages = main_agent.history.lock().unwrap().messages.clone();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[2].role, Role::Tool);
    let is_err = messages[2]
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. }));
    assert!(!is_err);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_03_permission_ask_in_build_mode() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

    let session_id = core
        .open_session(Attach::New {
            cwd,
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: Some(Mode::Build),
        })
        .await
        .unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "write".into(),
            serde_json::json!({"path": "out.txt", "content": "hello file"}),
        )],
    });
    mock.push(MockResponse::Text("File written!".into()));

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "write out.txt".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);

    let perm_id = loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for permission request"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                if let Event::PermissionRequested(req) = event {
                    break req.id;
                }
            }
        }
    };
    core.handle(
        &session_id,
        Request::PermissionReply {
            permission: perm_id,
            decision: PermissionDecision::AllowOnce,
        },
    )
    .await;

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn to finish"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                if let Event::TurnEnded { .. } = event {
                    break;
                }
            }
        }
    }

    assert!(
        tokio::fs::metadata(_tmp.path().join("out.txt"))
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_04_injection_at_point_d() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

    let session_id = core
        .open_session(Attach::New {
            cwd,
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: Some(Mode::Bypass),
        })
        .await
        .unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("trigger_bg".into(), serde_json::json!({}))],
    });
    mock.push(MockResponse::Text("Background task noticed".into()));

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "start background task".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(4));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn to finish"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                if let Event::TurnEnded { .. } = event {
                    break;
                }
            }
        }
    }

    let reqs = mock.requests();
    assert!(reqs.len() >= 2);
    let second_req = &reqs[1];
    let has_task_finished = second_req
        .messages
        .iter()
        .any(|m| m.text().contains("<task_finished"));
    assert!(
        has_task_finished,
        "second request should contain injected task_finished"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_05_subagent_spawn_and_finish() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

    let session_id = core
        .open_session(Attach::New {
            cwd,
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: Some(Mode::Bypass),
        })
        .await
        .unwrap();

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "agent".into(),
            serde_json::json!({"prompt": "subtask prompt", "name": "sub-1"}),
        )],
    });
    mock.push(MockResponse::Text("Spawned subagent".into()));
    mock.push(MockResponse::Text("subagent completed successfully".into()));
    mock.push(MockResponse::Text(
        "Acknowledged subagent completion".into(),
    ));
    // The session-title request after the first reply consumes one script too.
    mock.push(MockResponse::Text("Delegated subtask".into()));

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "delegate work".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    let mut main_saw_agent_finished = false;

    loop {
        tokio::select! {
            _ = &mut timeout => break,
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { agent, .. })) = res
                    && agent.is_main()
                {
                    let reqs = mock.requests();
                    if reqs
                        .iter()
                        .any(|r| r.messages.iter().any(|m| m.text().contains("<agent_finished")))
                    {
                        main_saw_agent_finished = true;
                        break;
                    }
                }
            }
        }
    }

    if !main_saw_agent_finished {
        for (i, r) in mock.requests().iter().enumerate() {
            let last = r.messages.last().map(|m| m.text()).unwrap_or_default();
            eprintln!(
                "request {i}: {} messages, last = {last:?}",
                r.messages.len()
            );
        }
        eprintln!("pending scripts: {}", mock.pending());
        for a in core.session(&session_id).unwrap().agent_infos() {
            eprintln!("agent {} {:?} {:?}", a.name, a.status, a.error);
        }
    }
    assert!(
        main_saw_agent_finished,
        "main agent requests should contain <agent_finished"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_06_interrupt_mid_stream() {
    let (core, mock, _tmp) = setup_test_core().await;
    let cwd = _tmp.path().to_path_buf();

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

    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(100),
        text: "streaming text that will take a bit...".into(),
    });

    let mut rx = core.subscribe(&session_id).unwrap();

    core.handle(
        &session_id,
        Request::UserMessage {
            text: "long request".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for delta"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TextDelta { .. })) = res {
                    break;
                }
            }
        }
    }

    core.handle(&session_id, Request::Interrupt).await;

    let mut saw_interrupted = false;
    loop {
        tokio::select! {
            _ = &mut timeout => break,
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { stop, .. })) = res
                    && stop == TurnStop::Interrupted
                {
                    saw_interrupted = true;
                    break;
                }
            }
        }
    }

    assert!(saw_interrupted);

    let session = core.session(&session_id).unwrap();
    let main_agent = session.main_agent().unwrap();
    let messages = main_agent.history.lock().unwrap().messages.clone();
    assert!(!messages.is_empty());
    assert_eq!(messages[0].role, Role::User);
}
