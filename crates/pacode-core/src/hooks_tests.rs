//! Tests for `hooks`: matcher semantics, exit-code handling, timeout fail-open,
//! env/stdin plumbing, and the detached lifecycle events.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pacode_types::{AgentId, AgentInfo, Config, Event, HookRule, ModelRoute, TranscriptKind};

use super::*;

fn rule(matcher: &str, command: &str, timeout_secs: u64) -> HookRule {
    HookRule {
        matcher: matcher.to_string(),
        command: command.to_string(),
        timeout_secs,
    }
}

fn ctx(tool: Option<&str>) -> HookCtx {
    HookCtx {
        event: PRE_TOOL_USE,
        tool: tool.map(str::to_string),
        session_id: "test-session".to_string(),
        cwd: std::env::temp_dir(),
        input: Some(serde_json::json!({"command": "ls"})),
        output: None,
    }
}

/// Smallest live `Session` a hook can run against (store in memory, spool in
/// the temp dir the caller keeps alive).
fn test_session(config: Config) -> (tempfile::TempDir, Arc<Session>, Arc<Agent>) {
    let temp = tempfile::tempdir().unwrap();
    let id = pacode_types::SessionId::generate();
    let now = pacode_types::now_ms();
    let route = ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    };
    let meta = pacode_types::SessionMeta {
        id: id.clone(),
        name: None,
        cwd: temp.path().to_path_buf(),
        git_branch: None,
        created_at_ms: now,
        updated_at_ms: now,
        model: route.clone(),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
    };
    let info = AgentInfo {
        id: AgentId::main(),
        name: "main".to_string(),
        kind: pacode_types::AgentKind::Main,
        status: pacode_types::AgentStatus::Idle,
        activity: None,
        started_at_ms: now,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: route,
        effort: pacode_types::Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    };
    let tools = pacode_tools::ToolRegistry::new();
    let agent = Arc::new(Agent::new(
        AgentId::main(),
        info,
        crate::agent::History::default(),
        tools.clone(),
        200,
        None,
    ));
    let mut agents = BTreeMap::new();
    agents.insert(AgentId::main(), agent.clone());
    let session = Arc::new(Session {
        id,
        meta: RwLock::new(meta),
        agents: RwLock::new(agents),
        plan: RwLock::new(pacode_types::Plan::default()),
        usage: RwLock::new(pacode_types::UsageTotals::default()),
        permissions: crate::permissions::PermissionState::default(),
        questions: crate::questions::QuestionState::default(),
        events: crate::transcript::EventSink::new(),
        tasks: pacode_exec::TaskManager::new(
            temp.path().join("spool"),
            pacode_types::ExecConfig::default(),
        ),
        store: pacode_store::Store::open_in_memory().unwrap(),
        config: Arc::new(config),
        providers: Arc::new(pacode_provider::ProviderRegistry::empty()),
        tools: RwLock::new(tools),
        mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        app_version: "test".to_string(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
        scheduler: crate::schedule::Scheduler::new(),
    });
    (temp, session, agent)
}

#[test]
fn matcher_catch_all_and_regex() {
    assert!(matcher_matches("", "bash"));
    assert!(matcher_matches(".*", "bash"));
    assert!(matcher_matches("  ", "bash"));
    assert!(matcher_matches("bash", "bash"));
    assert!(matcher_matches("^ba", "bash"));
    assert!(matcher_matches("bash|edit", "edit"));
    assert!(!matcher_matches("edit", "bash"));
    // An invalid regex fails open: it matches nothing rather than everything.
    assert!(!matcher_matches("[invalid", "bash"));
}

#[test]
fn matching_filters_and_toolless_events() {
    let rules = vec![rule("bash", "a", 1), rule("", "b", 1)];
    assert_eq!(matching(&rules, Some("bash")).len(), 2);
    assert_eq!(matching(&rules, Some("edit")).len(), 1);
    // No tool name (session/notification events): only catch-alls run.
    assert_eq!(matching(&rules, None).len(), 1);
    assert_eq!(matching(&[], Some("bash")).len(), 0);
}

#[tokio::test]
async fn run_rule_captures_exit_code_and_stderr() {
    let c = ctx(Some("bash"));
    match run_rule(&rule("", "echo err-line >&2; exit 0", 5), &c).await {
        HookRun::Exited { code, stderr } => {
            assert_eq!(code, 0);
            assert_eq!(stderr, "err-line");
        }
        _ => panic!("expected exit"),
    }
    match run_rule(&rule("", "echo oops >&2; exit 7", 5), &c).await {
        HookRun::Exited { code, stderr } => {
            assert_eq!(code, 7);
            assert_eq!(stderr, "oops");
        }
        _ => panic!("expected exit"),
    }
}

#[tokio::test]
async fn run_rule_passes_env_and_stdin_json() {
    let c = ctx(Some("bash"));
    let run = run_rule(
        &rule(
            "",
            "echo \"$PACODE_HOOK_EVENT|$PACODE_HOOK_TOOL|$PACODE_HOOK_SESSION\" >&2; cat >&2",
            5,
        ),
        &c,
    )
    .await;
    match run {
        HookRun::Exited { code, stderr } => {
            assert_eq!(code, 0);
            assert!(stderr.contains("pre_tool_use|bash|test-session"));
            assert!(stderr.contains("\"event\":\"pre_tool_use\""));
            assert!(stderr.contains("\"session_id\":\"test-session\""));
            assert!(stderr.contains("\"tool\":\"bash\""));
            assert!(stderr.contains("\"input\":{\"command\":\"ls\"}"));
        }
        _ => panic!("expected exit"),
    }
}

#[tokio::test]
async fn run_rule_timeout_kills() {
    let c = ctx(Some("bash"));
    let start = std::time::Instant::now();
    let run = run_rule(&rule("", "sleep 30", 1), &c).await;
    assert!(matches!(run, HookRun::TimedOut { timeout_secs: 1 }));
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn run_rule_spawn_failure() {
    let mut c = ctx(Some("bash"));
    c.cwd = PathBuf::from("/nonexistent-dir-pacode-hooks-test");
    let run = run_rule(&rule("", "true", 5), &c).await;
    assert!(matches!(run, HookRun::Failed(_)));
}

#[tokio::test]
async fn pre_exit_zero_allows() {
    let c = ctx(Some("bash"));
    let (decision, warnings) = eval_pre(&[rule("", "exit 0", 5)], "bash", &c).await;
    assert!(matches!(decision, PreToolDecision::Allow));
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn pre_empty_rules_allow() {
    let c = ctx(Some("bash"));
    let (decision, warnings) = eval_pre(&[], "bash", &c).await;
    assert!(matches!(decision, PreToolDecision::Allow));
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn pre_exit_two_blocks_with_stderr() {
    let c = ctx(Some("bash"));
    let (decision, _) = eval_pre(&[rule("", "echo no-way >&2; exit 2", 5)], "bash", &c).await;
    match decision {
        PreToolDecision::Block(reason) => assert_eq!(reason, "no-way"),
        PreToolDecision::Allow => panic!("expected block"),
    }
}

#[tokio::test]
async fn pre_exit_two_without_stderr_uses_fallback() {
    let c = ctx(Some("bash"));
    let (decision, _) = eval_pre(&[rule("", "exit 2", 5)], "bash", &c).await;
    match decision {
        PreToolDecision::Block(reason) => assert!(reason.contains("exit 2")),
        PreToolDecision::Allow => panic!("expected block"),
    }
}

#[tokio::test]
async fn pre_nonzero_warns_and_allows() {
    let c = ctx(Some("bash"));
    let (decision, warnings) =
        eval_pre(&[rule("", "echo bad-input >&2; exit 1", 5)], "bash", &c).await;
    assert!(matches!(decision, PreToolDecision::Allow));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("bad-input"));
    assert!(warnings[0].contains("call allowed"));
}

#[tokio::test]
async fn pre_timeout_is_fail_open() {
    let c = ctx(Some("bash"));
    let (decision, warnings) = eval_pre(&[rule("", "sleep 30", 1)], "bash", &c).await;
    assert!(matches!(decision, PreToolDecision::Allow));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("timed out"));
}

#[tokio::test]
async fn pre_nonmatching_rule_never_runs() {
    let c = ctx(Some("bash"));
    // `exit 2` would block if the rule ran.
    let (decision, warnings) = eval_pre(&[rule("edit", "exit 2", 5)], "bash", &c).await;
    assert!(matches!(decision, PreToolDecision::Allow));
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn post_nonzero_warns_and_never_blocks() {
    let c = ctx(Some("bash"));
    // Exit code 2 has no special meaning post-call: it is just a warning.
    let warnings = eval_post(&[rule("", "echo late >&2; exit 2", 5)], "bash", &c).await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("late"));
}

#[tokio::test]
async fn pre_tool_use_emits_notice_and_blocks() {
    let mut config = Config::default();
    config.hooks.pre_tool_use = vec![
        rule("", "echo warn-me >&2; exit 1", 5),
        rule("", "echo stop >&2; exit 2", 5),
    ];
    let (_tmp, session, agent) = test_session(config);
    let mut rx = session.events.subscribe();
    let decision = pre_tool_use(&session, &agent, "bash", &serde_json::json!({})).await;
    match decision {
        PreToolDecision::Block(reason) => assert_eq!(reason, "stop"),
        PreToolDecision::Allow => panic!("expected block"),
    }
    let (_, event) = rx.try_recv().unwrap();
    match event {
        Event::ItemAdded(item) => match item.kind {
            TranscriptKind::Notice { level, text } => {
                assert_eq!(level, pacode_types::ToastLevel::Warn);
                assert!(text.contains("warn-me"));
            }
            other => panic!("expected notice, got {other:?}"),
        },
        other => panic!("expected ItemAdded, got {other:?}"),
    }
}

/// Wait for a marker file a detached hook writes; returns its contents.
async fn wait_for_file(path: &std::path::Path) -> String {
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(path) {
            return content;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("hook never wrote {}", path.display());
}

#[tokio::test]
async fn session_start_fires_once_per_session() {
    let mut config = Config::default();
    config.hooks.session_start = vec![rule("", "echo hi >> \"$PACODE_HOOK_CWD/started\"", 5)];
    let (_tmp, session, agent) = test_session(config);
    let marker = session.meta().cwd.join("started");

    session_start(&session, &agent);
    session_start(&session, &agent);

    assert_eq!(wait_for_file(&marker).await, "hi\n");
    // Give a wrongly-fired second hook a moment to double-append.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "hi\n");
}

#[tokio::test]
async fn session_end_and_notification_fire_detached() {
    let mut config = Config::default();
    config.hooks.session_end = vec![rule("", "echo bye > \"$PACODE_HOOK_CWD/ended\"", 5)];
    config.hooks.notification = vec![
        rule("", "echo note > \"$PACODE_HOOK_CWD/noted\"", 5),
        rule("zzz-nomatch", "echo no > \"$PACODE_HOOK_CWD/skipped\"", 5),
    ];
    let (_tmp, session, _agent) = test_session(config);
    let cwd = session.meta().cwd;

    session_end(&session);
    notification(&session, &AgentId::main(), Some("bash".to_string()));

    assert_eq!(wait_for_file(&cwd.join("ended")).await, "bye\n");
    assert_eq!(wait_for_file(&cwd.join("noted")).await, "note\n");
    assert!(!cwd.join("skipped").exists());
}
