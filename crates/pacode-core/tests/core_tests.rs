use std::collections::HashSet;
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
    AgentId, Attach, Config, ContentBlock, Event, Mode, ModelRoute, PermissionDecision,
    ProgressSource, QuestionAnswer, Reply, Request, RiskLevel, Role, SessionId, SessionStatusKind,
    TaskProgress, ToolStatus, TranscriptKind, TurnStop,
};
use tokio::sync::Barrier;

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
        ctx.host
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
            .await
            .into_result("user denied execution")?;
        Ok(ToolOutput::text("hi\n"))
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
        ctx.host
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
            .await
            .into_result("user denied write")?;
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
                exclude_tools: None,
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

/// Drives the `ask_question` host path so slot-status tests can exercise the
/// `NeedsInput` transition without a permission gate.
struct StubAsk;
#[async_trait::async_trait]
impl Tool for StubAsk {
    fn name(&self) -> &'static str {
        "ask"
    }
    fn description(&self) -> &'static str {
        "Ask the user a question"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let answer = ctx
            .host
            .ask_question(
                &ctx.call_id,
                "pick".into(),
                "which one?".into(),
                vec![
                    pacode_types::QuestionOption::new("red", ""),
                    pacode_types::QuestionOption::new("blue", ""),
                ],
                false,
            )
            .await?;
        Ok(ToolOutput::text(format!(
            "answered: {}",
            answer.selected.len()
        )))
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
    tools.register(Arc::new(StubAsk));

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
            images: Vec::new(),
            message_id: None,
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
            images: Vec::new(),
            message_id: None,
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
async fn user_steer_during_tool_call_commits_after_tool_result() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_id = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: Some(Mode::Build),
        })
        .await
        .unwrap();
    core.session(&session_id)
        .unwrap()
        .meta
        .write()
        .unwrap()
        .name = Some("Steer test".into());

    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "write".into(),
            serde_json::json!({"path": "steer.txt", "content": "done"}),
        )],
    });
    mock.push(MockResponse::Text("finished after steer".into()));

    let mut rx = core.subscribe(&session_id).unwrap();
    assert!(matches!(
        core.handle(
            &session_id,
            Request::UserMessage {
                text: "start tool call".into(),
                images: Vec::new(),
                message_id: Some("msg-tool-call".into()),
            },
        )
        .await,
        Reply::Ok
    ));

    let permission_id = {
        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                _ = &mut timeout => panic!("timed out waiting for permission request"),
                res = rx.recv() => {
                    if let Ok((_seq, Event::PermissionRequested(request))) = res {
                        break request.id;
                    }
                }
            }
        }
    };

    let session = core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    assert!(matches!(
        core.handle(
            &session_id,
            Request::UserMessage {
                text: "change direction".into(),
                images: Vec::new(),
                message_id: Some("msg-tool-steer".into()),
            },
        )
        .await,
        Reply::Ok
    ));

    {
        let history = main.history.lock().unwrap();
        assert_eq!(history.messages.len(), 2);
        assert_eq!(history.messages[0].role, Role::User);
        assert_eq!(history.messages[1].role, Role::Assistant);
        assert!(history.messages[1].has_tool_uses());
        assert!(
            history
                .messages
                .iter()
                .all(|message| message.text() != "change direction")
        );
    }
    assert!(!main.injections.is_empty());

    assert!(matches!(
        core.handle(
            &session_id,
            Request::PermissionReply {
                permission: permission_id,
                decision: PermissionDecision::AllowOnce,
                reason: None,
            },
        )
        .await,
        Reply::Ok
    ));

    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    let mut completed_turns = 0;
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn completion"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { stop, .. })) = res {
                    assert_eq!(stop, TurnStop::Completed);
                    completed_turns += 1;
                    if completed_turns == 2 {
                        break;
                    }
                }
            }
        }
    }

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let steer_requests = requests
        .iter()
        .filter(|request| {
            request
                .messages
                .iter()
                .any(|message| message.role == Role::User && message.text() == "change direction")
        })
        .collect::<Vec<_>>();
    assert_eq!(steer_requests.len(), 1);
    let next = steer_requests[0];
    let tool_use_index = next
        .messages
        .iter()
        .position(|message| message.tool_uses().any(|(_, name, _)| name == "write"))
        .expect("tool_use must reach the next provider request");
    let tool_result_index = next
        .messages
        .iter()
        .position(|message| message.role == Role::Tool)
        .expect("tool_result must reach the next provider request");
    let steer_index = next
        .messages
        .iter()
        .position(|message| message.role == Role::User && message.text() == "change direction")
        .expect("steer must reach the next provider request");
    assert_eq!(tool_result_index, tool_use_index + 1);
    assert_eq!(steer_index, tool_result_index + 1);
    assert_eq!(
        next.messages
            .iter()
            .filter(|message| message.role == Role::User && message.text() == "change direction")
            .count(),
        1
    );
    assert!(
        next.messages
            .iter()
            .all(|message| !message.text().contains("<user_message>"))
    );
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
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    let perm_id = loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for permission request"),
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
            reason: None,
        },
    )
    .await;

    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for turn to finish"),
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
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(4));
    tokio::pin!(timeout);

    let mut task_ended = false;
    let mut toasts = Vec::new();

    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn to finish"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                match &event {
                    Event::TaskUpdated(info) if info.status.is_terminal() => task_ended = true,
                    Event::Toast { title, .. } => toasts.push(title.clone()),
                    Event::TurnEnded { .. } if task_ended => break,
                    _ => {}
                }
            }
        }
    }

    // A finished task is reported once, as the transcript line the client builds
    // from `TaskUpdated`. A toast on top of it said the same thing twice.
    assert!(task_ended, "the task must be reported as ended");
    assert!(
        toasts.is_empty(),
        "a finished task must not also raise a toast: {toasts:?}"
    );

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
            images: Vec::new(),
            message_id: None,
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
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    loop {
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for delta"),
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
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);
        tokio::select! {
            _ = &mut deadline => break,
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

/// End-to-end: a tool parked on a permission prompt gets a `Deny` with the
/// `interrupted` reason when Ctrl+C lands, the pending map is cleaned, and the
/// turn ends `Interrupted` instead of hanging forever.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_during_permission_wait_denies_and_unwinds() {
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
            serde_json::json!({"path": "a.txt", "content": "x"}),
        )],
    });

    let mut rx = core.subscribe(&session_id).unwrap();
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "write a file".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    // Wait until the tool actually parks on the permission prompt.
    let perm_id = {
        let timeout = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                _ = &mut timeout => panic!("timed out waiting for permission request"),
                res = rx.recv() => {
                    if let Ok((_seq, Event::PermissionRequested(req))) = res {
                        break req.id;
                    }
                }
            }
        }
    };

    core.handle(&session_id, Request::Interrupt).await;

    let mut saw_resolved = false;
    let mut saw_interrupted_end = false;
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => break,
            res = rx.recv() => {
                match res {
                    Ok((_seq, Event::PermissionResolved { permission, decision, reason })) => {
                        assert_eq!(permission, perm_id);
                        assert_eq!(decision, PermissionDecision::Deny);
                        assert_eq!(
                            reason.as_deref(),
                            Some("interrupted"),
                            "interrupt must explain the denial"
                        );
                        saw_resolved = true;
                    }
                    Ok((_seq, Event::TurnEnded {
                        stop: TurnStop::Interrupted,
                        ..
                    })) => {
                        saw_interrupted_end = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(saw_resolved, "PermissionResolved must be emitted");
    assert!(saw_interrupted_end, "turn must end Interrupted");

    let session = core.session(&session_id).unwrap();
    assert!(
        session.permissions.pending().is_empty(),
        "interrupt must clear pending permissions"
    );
}

/// End-to-end: `ExplainPermission` answers `Reply::Ok` for a pending request,
/// runs the model call detached, and broadcasts `Event::PermissionExplained`
/// with the parsed level. Unknown ids error, and the explanation never
/// resolves the permission itself.
#[tokio::test(flavor = "multi_thread")]
async fn explain_permission_runs_detached_and_broadcasts() {
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

    // Script order: the turn's ToolCalls, then the detached explain call's
    // LEVEL/EXPLANATION reply, then the post-tool answer.
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "write".into(),
            serde_json::json!({"path": "out.txt", "content": "data"}),
        )],
    });
    mock.push(MockResponse::Text(
        "LEVEL: high\nEXPLANATION: Writes a file, which can clobber existing content.".into(),
    ));
    mock.push(MockResponse::Text("done".into()));

    let mut rx = core.subscribe(&session_id).unwrap();
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "write out.txt".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    let perm_id = {
        let timeout = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                _ = &mut timeout => panic!("timed out waiting for permission request"),
                res = rx.recv() => {
                    if let Ok((_seq, Event::PermissionRequested(req))) = res {
                        break req.id;
                    }
                }
            }
        }
    };

    // Unknown or already-resolved ids fail synchronously.
    let reply = core
        .handle(
            &session_id,
            Request::ExplainPermission {
                permission: pacode_types::PermissionId::new("perm_nope"),
            },
        )
        .await;
    assert!(
        matches!(reply, pacode_types::Reply::Error { .. }),
        "unknown permission id must error"
    );

    let reply = core
        .handle(
            &session_id,
            Request::ExplainPermission {
                permission: perm_id.clone(),
            },
        )
        .await;
    assert!(matches!(reply, pacode_types::Reply::Ok));

    // The detached task resolves the route, calls the provider, parses the
    // reply, and broadcasts — the test only waits on the event.
    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for PermissionExplained"),
            res = rx.recv() => {
                if let Ok((_seq, Event::PermissionExplained { permission, text, level })) = res {
                    assert_eq!(permission, perm_id);
                    assert_eq!(level, Some(pacode_types::ExplainLevel::High));
                    assert!(text.contains("clobber"), "unexpected explain text: {text}");
                    break;
                }
            }
        }
    }

    // The explain call is display-only: the permission stays pending.
    let session = core.session(&session_id).unwrap();
    assert_eq!(session.permissions.pending().len(), 1);

    // The second recorded request is the explain call — its own system prompt,
    // not the turn's.
    let requests = mock.requests();
    assert!(
        requests[1].system_static.contains("permission explainer"),
        "expected the explain system prompt, got: {}",
        requests[1].system_static
    );

    core.handle(
        &session_id,
        Request::PermissionReply {
            permission: perm_id,
            decision: PermissionDecision::AllowOnce,
            reason: None,
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn end"),
            res = rx.recv() => {
                if let Ok((_seq, Event::TurnEnded { .. })) = res {
                    break;
                }
            }
        }
    }
}

/// Tool-call transcript cells appear at `ToolCallStart`, stream raw args via
/// `ItemUpdated`, and dispatch reuses the same seq — no second cell, and the
/// tool output replaces the args preview when the call lands.
#[tokio::test(flavor = "multi_thread")]
async fn tool_call_args_stream_into_the_transcript_cell() {
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

    // >256 bytes of args: the coalescer flushes inside the delta arm, so an
    // `ItemUpdated` lands mid-stream rather than only at dispatch.
    let command = format!("echo {}", "x".repeat(300));
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("bash".into(), serde_json::json!({ "command": command }))],
    });
    mock.push(MockResponse::Text("done".into()));

    let mut rx = core.subscribe(&session_id).unwrap();
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "run bash".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    // (added?, item seq, status, title, preview) per ToolCall cell event.
    let mut cell_events: Vec<(bool, u64, ToolStatus, String, String)> = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for turn end"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                let is_added = matches!(event, Event::ItemAdded(_));
                match event {
                    Event::ItemAdded(item) | Event::ItemUpdated(item) => {
                        if let TranscriptKind::ToolCall {
                            status, title, preview, ..
                        } = &item.kind
                        {
                            cell_events.push((
                                is_added,
                                item.seq,
                                *status,
                                title.clone(),
                                preview.clone(),
                            ));
                        }
                    }
                    Event::TurnEnded { .. } => break,
                    _ => {}
                }
            }
        }
    }

    let (added, first_seq, status, title, preview) = &cell_events[0];
    assert!(*added, "the cell arrives via ItemAdded at ToolCallStart");
    assert_eq!(*status, ToolStatus::Running);
    assert_eq!(
        title.as_str(),
        "bash",
        "title is the tool name until args parse"
    );
    assert!(preview.is_empty(), "no args have streamed yet");

    // Every later cell event reuses the same seq: dispatch emits
    // ItemUpdated, never a second ItemAdded.
    assert_eq!(cell_events.iter().filter(|(added, ..)| *added).count(), 1);
    assert!(cell_events.iter().all(|(_, seq, ..)| seq == first_seq));

    // Mid-stream: a Running update whose preview is the raw args JSON.
    assert!(
        cell_events
            .iter()
            .any(|(added, _, status, title, preview)| {
                !*added
                    && *status == ToolStatus::Running
                    && title == "bash"
                    && preview.contains("\"command\"")
            }),
        "args deltas must update the cell preview while Running"
    );

    // Dispatch: real title (args-derived), still Running, args preview kept.
    assert!(
        cell_events
            .iter()
            .any(|(added, _, status, title, preview)| {
                !*added
                    && *status == ToolStatus::Running
                    && title.starts_with("bash echo")
                    && preview.contains("\"command\"")
            }),
        "dispatch must refresh the title on the same cell"
    );

    // Final: the tool output replaces the args preview.
    let (_, _, status, _, preview) = cell_events.last().unwrap();
    assert_eq!(*status, ToolStatus::Ok);
    assert_eq!(preview.as_str(), "hi\n");
}

// ---- daemon-wide slot status (`Event::SessionStatus` on `subscribe_global`) ----

/// Next `SessionStatus` for `session` on the daemon-wide stream; everything
/// else (other events, broadcast lag) is skipped.
async fn next_session_status(
    rx: &mut tokio::sync::broadcast::Receiver<(u64, Event)>,
    session: &SessionId,
) -> SessionStatusKind {
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for SessionStatus"),
            res = rx.recv() => {
                if let Ok((_seq, Event::SessionStatus { session: s, status })) = res
                    && s == *session
                {
                    return status;
                }
            }
        }
    }
}

async fn open_status_session(core: &Arc<Core>, tmp: &tempfile::TempDir, mode: Mode) -> SessionId {
    core.open_session(Attach::New {
        cwd: tmp.path().to_path_buf(),
        model: Some(ModelRoute {
            provider: "mock".into(),
            model: "mock-model".into(),
        }),
        effort: None,
        mode: Some(mode),
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn session_status_marks_turn_working_then_done() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_id = open_status_session(&core, &tmp, Mode::Build).await;

    let mut global = core.subscribe_global();
    mock.push(MockResponse::Text("done".into()));
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "go".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Working
    );
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Done
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_status_needs_input_on_pending_permission() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_id = open_status_session(&core, &tmp, Mode::Build).await;

    let mut global = core.subscribe_global();
    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "write".into(),
            serde_json::json!({"path": "p.txt", "content": "x"}),
        )],
    });
    mock.push(MockResponse::Text("done".into()));
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "write it".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Working
    );

    // The permission request flips the badge to NeedsInput.
    let perm_id = loop {
        let (_seq, event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::PermissionRequested(req) = event {
            break req.id;
        }
    };
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::NeedsInput
    );

    // Resolved while the turn still runs: back to Working, then Done at the end.
    core.handle(
        &session_id,
        Request::PermissionReply {
            permission: perm_id,
            decision: PermissionDecision::AllowOnce,
            reason: None,
        },
    )
    .await;
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Working
    );
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Done
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_status_needs_input_on_pending_question() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_id = open_status_session(&core, &tmp, Mode::Build).await;

    let mut global = core.subscribe_global();
    let mut rx = core.subscribe(&session_id).unwrap();
    mock.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![("ask".into(), serde_json::json!({}))],
    });
    mock.push(MockResponse::Text("done".into()));
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "ask me".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Working
    );

    let question_id = loop {
        let (_seq, event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::QuestionAsked(q) = event {
            break q.id;
        }
    };
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::NeedsInput
    );

    core.handle(
        &session_id,
        Request::AnswerQuestion {
            question: question_id,
            answer: QuestionAnswer::choice(0),
        },
    )
    .await;
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Working
    );
    assert_eq!(
        next_session_status(&mut global, &session_id).await,
        SessionStatusKind::Done
    );
}

/// A second session's badge reaches the same global stream — the propagation
/// a client attached to slot A relies on to paint slot B.
#[tokio::test(flavor = "multi_thread")]
async fn session_status_reports_other_sessions() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_a = open_status_session(&core, &tmp, Mode::Build).await;
    let session_b = open_status_session(&core, &tmp, Mode::Build).await;

    let mut global = core.subscribe_global();
    mock.push(MockResponse::Text("b done".into()));
    core.handle(
        &session_b,
        Request::UserMessage {
            text: "go".into(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    // B's Working arrives tagged with B — while A never ran a turn.
    assert_eq!(
        next_session_status(&mut global, &session_b).await,
        SessionStatusKind::Working
    );
    assert_eq!(
        next_session_status(&mut global, &session_b).await,
        SessionStatusKind::Done
    );
    // And A stayed silent on the stream.
    assert_ne!(session_a, session_b);
}

#[tokio::test(flavor = "multi_thread")]
async fn preallocated_new_attach_reuses_the_same_session() {
    let (core, _mock, tmp) = setup_test_core().await;
    let attach = Attach::New {
        cwd: tmp.path().to_path_buf(),
        model: Some(ModelRoute::new("mock", "mock-model")),
        effort: None,
        mode: None,
    };
    let session_id = SessionId::new("pacode-attach-test-idempotent");

    let first = core
        .open_session_with_id(attach.clone(), session_id.clone())
        .await
        .unwrap();
    let second = core
        .open_session_with_id(attach, session_id.clone())
        .await
        .unwrap();

    assert_eq!(first, session_id);
    assert_eq!(second, session_id);
    assert_eq!(
        core.session_ids()
            .into_iter()
            .filter(|id| id == &session_id)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn preallocated_latest_attach_stays_bound_after_newer_session() {
    let (core, _mock, tmp) = setup_test_core().await;
    let first = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let identity = SessionId::new("pacode-attach-test-latest-idempotent");
    let bound = core
        .open_session_with_id(
            Attach::Latest {
                cwd: tmp.path().to_path_buf(),
            },
            identity.clone(),
        )
        .await
        .unwrap();
    assert_eq!(bound, first);

    let first_updated_at = core.session(&first).unwrap().meta().updated_at_ms;
    while pacode_types::now_ms() <= first_updated_at {
        tokio::task::yield_now().await;
    }
    let newer = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    assert_eq!(
        core.open_session(Attach::Latest {
            cwd: tmp.path().to_path_buf(),
        })
        .await
        .unwrap(),
        newer
    );

    let retried = core
        .open_session_with_id(
            Attach::Latest {
                cwd: tmp.path().to_path_buf(),
            },
            identity,
        )
        .await
        .unwrap();
    assert_eq!(retried, first);
    assert_eq!(core.session_ids().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_resume_keeps_one_live_session() {
    let (core, _mock, tmp) = setup_test_core().await;
    let session_id = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    core.close_session(&session_id).await;

    let (first, second) = tokio::join!(
        core.open_session(Attach::Resume {
            session: session_id.clone(),
        }),
        core.open_session(Attach::Resume {
            session: session_id.clone(),
        })
    );

    assert_eq!(first.unwrap(), session_id);
    assert_eq!(second.unwrap(), session_id);
    assert_eq!(core.session_ids().len(), 1);
    assert!(core.session(&session_id).is_some());
}

#[tokio::test]
async fn task_router_recovers_after_lag_and_keeps_routing() {
    let (core, mock, tmp) = setup_test_core().await;
    let session_id = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = core.session(&session_id).unwrap();
    let tasks = session.tasks.clone();
    let mut events = core.subscribe(&session_id).unwrap();
    // Backgrounded: a foreground task's end is reported by the call waiting on
    // it, so only a background one reaches the model as `<task_finished>`.
    let mut spec = TaskSpec::new(session_id.clone(), AgentId::main(), "sleep 30", tmp.path());
    spec.background = true;
    let first = tasks.spawn(spec).await.unwrap();
    tokio::task::yield_now().await;

    for current in 0..300 {
        tasks
            .report_progress(
                &first.id,
                TaskProgress {
                    current: Some(current),
                    total: Some(300),
                    percent: None,
                    message: None,
                    source: ProgressSource::Reported,
                    updated_at_ms: pacode_types::now_ms(),
                },
            )
            .unwrap();
    }

    mock.push(MockResponse::Text("task result received".into()));
    tasks.kill(&first.id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if mock.requests().iter().any(|request| {
                request.messages.iter().any(|message| {
                    message
                        .text()
                        .contains(&format!("<task_finished id=\"{}\"", first.id))
                })
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task router must recover TaskFinished after Lagged");

    let second = tasks
        .spawn(TaskSpec::new(
            session_id.clone(),
            AgentId::main(),
            "sleep 30",
            tmp.path(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                events.recv().await,
                Ok((_, Event::TaskAdded(info))) if info.id == second.id
            ) {
                break;
            }
        }
    })
    .await
    .expect("task router must keep receiving after Lagged");
    tasks.kill(&second.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn w128_agent_admission_and_fanout() {
    const W128: usize = 128;

    let (core, mock, tmp) = setup_test_core().await;
    let session_id = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: Some(Mode::Bypass),
        })
        .await
        .unwrap();
    for _ in 0..(W128 * 4) {
        mock.push(MockResponse::Text("agent finished".into()));
    }

    let session = core.session(&session_id).unwrap();
    let barrier = Arc::new(Barrier::new(W128));
    let mut spawns = Vec::with_capacity(W128);
    for index in 0..W128 {
        let session = session.clone();
        let barrier = barrier.clone();
        spawns.push(tokio::spawn(async move {
            barrier.wait().await;
            session
                .spawn_agent(AgentSpec {
                    prompt: format!("w128-{index}"),
                    name: Some(format!("w128-{index}")),
                    model: None,
                    effort: None,
                    fork: false,
                    tools: Some(Vec::new()),
                    exclude_tools: None,
                })
                .await
        }));
    }

    let mut ids = Vec::with_capacity(W128);
    for spawn in spawns {
        ids.push(spawn.await.unwrap().unwrap());
    }

    let finished = Arc::new(tokio::sync::Notify::new());
    let watcher_notify = finished.clone();
    let watcher_session = session.clone();
    let watcher = tokio::spawn(async move {
        loop {
            let infos = watcher_session.agent_infos();
            let subagents = infos
                .iter()
                .filter(|info| !info.id.is_main())
                .collect::<Vec<_>>();
            if subagents.len() == W128
                && subagents
                    .iter()
                    .all(|info| info.status == pacode_types::AgentStatus::Finished)
            {
                watcher_notify.notify_one();
                return;
            }
            tokio::task::yield_now().await;
        }
    });
    tokio::time::timeout(Duration::from_secs(20), finished.notified())
        .await
        .expect("128 agents must finish");
    watcher.await.unwrap();

    let infos = session.agent_infos();
    assert_eq!(infos.len(), W128 + 1);
    assert_eq!(
        infos
            .iter()
            .filter(|info| !info.id.is_main())
            .filter(|info| info.status == pacode_types::AgentStatus::Finished)
            .count(),
        W128
    );
    let unique_ids = ids.iter().collect::<HashSet<_>>();
    assert_eq!(unique_ids.len(), W128);
    let requests = mock.requests();
    for index in 0..W128 {
        let prompt = format!("w128-{index}");
        assert!(requests.iter().any(|request| {
            request
                .messages
                .iter()
                .any(|message| message.text() == prompt)
        }));
    }

    core.shutdown().await;
}
