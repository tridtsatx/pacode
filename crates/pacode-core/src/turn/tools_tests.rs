use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use pacode_tools::registry::MAX_TOOL_OUTPUT_BYTES;
use pacode_tools::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, ToolRegistry};
use pacode_types::{
    AgentId, AgentInfo, CallId, Config, ContentBlock, HookRule, ModelRoute, Role, SessionMeta,
    ToolStatus, TranscriptKind,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    BudgetLimits, ToolCallInfo, TurnBudget, execute_tool_calls, run_single_tool,
    split_result_for_consumers, turn_budget,
};
use crate::agent::{Agent, History};
use crate::session::Session;
use crate::turn::stream::PendingToolCall;

struct TestTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for TestTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "test"
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let label = input
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("default");
        let slow = label == "slow"
            || input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| path == "slow");
        if slow {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        let content = if label == "oversized" {
            "界".repeat(MAX_TOOL_OUTPUT_BYTES)
        } else {
            format!("{label}-result")
        };
        Ok(ToolOutput::text(content))
    }
}

fn pending(label: &str) -> PendingToolCall {
    PendingToolCall {
        call_id: CallId::new(format!("call-{label}")),
        name: "read".to_string(),
        input: serde_json::json!({"label": label, "intent": label}),
        parse_err: None,
        item_seq: None,
    }
}

fn pending_batch(index: usize) -> PendingToolCall {
    let ops = (0..8)
        .map(|_| json!({"tool": "read", "input": {"path": "slow"}}))
        .collect::<Vec<_>>();
    PendingToolCall {
        call_id: CallId::new(format!("call-batch-{index}")),
        name: "batch".to_string(),
        input: json!({"ops": ops, "intent": "fanout"}),
        parse_err: None,
        item_seq: None,
    }
}

fn test_session(
    config: Config,
    tool: Arc<dyn Tool>,
) -> (tempfile::TempDir, Arc<Session>, Arc<Agent>) {
    let temp = tempfile::tempdir().unwrap();
    let session_id = pacode_types::SessionId::new("tools-test-session");
    let agent_id = AgentId::main();
    let now = pacode_types::now_ms();
    let route = ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    };
    let meta = SessionMeta {
        id: session_id.clone(),
        name: None,
        cwd: temp.path().to_path_buf(),
        git_branch: None,
        created_at_ms: now,
        updated_at_ms: now,
        model: route.clone(),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: false,
    };
    let info = AgentInfo {
        id: agent_id.clone(),
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
    let tools = ToolRegistry::new().with(tool);
    let agent = Arc::new(Agent::new(
        agent_id.clone(),
        info,
        History::default(),
        tools.clone(),
        200,
        None,
    ));
    let mut agents = BTreeMap::new();
    agents.insert(agent_id, agent.clone());
    let session = Arc::new(Session {
        id: session_id,
        meta: RwLock::new(meta),
        agents: RwLock::new(agents),
        agent_aliases: std::sync::Mutex::new(HashMap::new()),
        plan: RwLock::new(Default::default()),
        usage: RwLock::new(Default::default()),
        permissions: crate::permissions::PermissionState::default(),
        questions: crate::questions::QuestionState::default(),
        events: crate::transcript::EventSink::new(),
        tasks: pacode_exec::TaskManager::new(
            temp.path().join("spool"),
            pacode_types::ExecConfig::default(),
        ),
        accounting: std::sync::Arc::new(crate::analyze::SessionAccounting::new()),
        store: pacode_store::Store::open_in_memory().unwrap(),
        submit_lock: tokio::sync::Mutex::new(()),
        closed: std::sync::atomic::AtomicBool::new(false),
        recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
        config: Arc::new(config),
        providers: Arc::new(pacode_provider::ProviderRegistry::empty()),
        tools: RwLock::new(tools),
        mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        app_version: "test".to_string(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
        scheduler: crate::schedule::Scheduler::new(),
        model_flags: RwLock::new(HashMap::new()),
        global_events: crate::transcript::EventSink::new(),
        idle_watch: crate::core::IdleWatch::default(),
    });
    (temp, session, agent)
}

fn tool_results(agent: &Agent) -> Vec<String> {
    agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| {
            message.content.iter().find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_results_commit_in_call_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, session, agent) = test_session(
        Config::default(),
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );

    let interrupted = execute_tool_calls(
        &session,
        &agent,
        &CancellationToken::new(),
        vec![pending("slow"), pending("fast")],
    )
    .await;

    assert!(!interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(tool_results(&agent), ["slow-result", "fast-result"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_batches_do_not_deadlock_child_workers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, session, agent) = test_session(
        Config::default(),
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );
    agent
        .tools
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .register(Arc::new(pacode_tools::builtin::batch::BatchTool::default()));
    let interrupted = tokio::time::timeout(
        Duration::from_secs(5),
        execute_tool_calls(
            &session,
            &agent,
            &CancellationToken::new(),
            (0..4).map(pending_batch).collect(),
        ),
    )
    .await
    .unwrap();

    assert!(!interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 32);
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_tool_output_is_capped_in_history() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.context.tool_output_cap_chars = 1_000_000;
    let (_temp, session, agent) = test_session(
        config,
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );

    execute_tool_calls(
        &session,
        &agent,
        &CancellationToken::new(),
        vec![pending("oversized")],
    )
    .await;

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1);
    assert!(results[0].len() <= MAX_TOOL_OUTPUT_BYTES);
    assert!(results[0].contains("output truncated"));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_interrupts_pre_tool_hook() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.hooks.pre_tool_use = vec![HookRule {
        matcher: String::new(),
        command: "sleep 30".to_string(),
        timeout_secs: 30,
    }];
    let (_temp, session, agent) = test_session(
        config,
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );
    let cancel = CancellationToken::new();
    let execution = tokio::spawn({
        let cancel = cancel.clone();
        let session = session.clone();
        let agent = agent.clone();
        async move { execute_tool_calls(&session, &agent, &cancel, vec![pending("hook")]).await }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let interrupted = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .unwrap()
        .unwrap();

    assert!(interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_results(&agent), ["cancelled"]);
    let item = agent
        .transcript
        .lock()
        .unwrap()
        .tail
        .back()
        .cloned()
        .unwrap();
    assert!(matches!(
        &item.kind,
        TranscriptKind::ToolCall {
            status: ToolStatus::Error,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_interrupts_post_tool_hook() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.hooks.post_tool_use = vec![HookRule {
        matcher: String::new(),
        command: "sleep 30".to_string(),
        timeout_secs: 30,
    }];
    let (_temp, session, agent) = test_session(
        config,
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );
    let cancel = CancellationToken::new();
    let execution = tokio::spawn({
        let cancel = cancel.clone();
        let session = session.clone();
        let agent = agent.clone();
        async move { execute_tool_calls(&session, &agent, &cancel, vec![pending("hook")]).await }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let interrupted = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .unwrap()
        .unwrap();

    assert!(interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_results(&agent), ["hook-result"]);
    let item = agent
        .transcript
        .lock()
        .unwrap()
        .tail
        .back()
        .cloned()
        .unwrap();
    assert!(matches!(
        &item.kind,
        TranscriptKind::ToolCall {
            status: ToolStatus::Ok,
            ..
        }
    ));
}

struct ExecTool;

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "test"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("ran"))
    }
}

static BUDGET_CALL_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Holds every call open for `hold` and records how many ran at once, so a test
/// can tell real parallelism from a queue.
struct ProbeTool {
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    hold: Duration,
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "test"
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.hold).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("probe-result"))
    }
}

/// A subagent alongside the session's main agent, sharing its tool registry.
fn subagent_of(session: &Session, main: &Agent, id: &str) -> Arc<Agent> {
    let agent_id = AgentId::new(id);
    let info = AgentInfo {
        id: agent_id.clone(),
        name: id.to_string(),
        kind: pacode_types::AgentKind::Sub,
        parent: Some(main.id()),
        started_at_ms: pacode_types::now_ms(),
        ..main.info()
    };
    let agent = Arc::new(Agent::new(
        agent_id.clone(),
        info,
        History::default(),
        main.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        200,
        None,
    ));
    session
        .agents
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(agent_id, agent.clone());
    agent
}

/// Regression guard for the removed process-wide worker cap, exercised through
/// the real dispatch path rather than the semaphore directly. 24 agents is above
/// the 16 that used to be global: under that cap 16 calls would have overlapped
/// and the rest would have waited, and at 128 subagents the parent's own
/// `agent spawn` call queued behind the children it had just spawned.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_agents_do_not_queue_behind_a_process_wide_cap() {
    const AGENTS: usize = 24;

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (_temp, session, main) = test_session(
        Config::default(),
        Arc::new(ProbeTool {
            active: active.clone(),
            peak: peak.clone(),
            hold: Duration::from_millis(150),
        }),
    );
    let agents = (0..AGENTS)
        .map(|index| subagent_of(&session, &main, &format!("sub-{index}")))
        .collect::<Vec<_>>();

    let results = tokio::time::timeout(
        Duration::from_secs(10),
        futures::future::join_all(agents.iter().map(|agent| {
            let session = session.clone();
            let agent = agent.clone();
            async move {
                call_with_budget(
                    &session,
                    &agent,
                    turn_budget(&session, &agent),
                    BudgetLimits {
                        tool_calls: 8,
                        exec_calls: 0,
                    },
                    "read",
                    "probe",
                )
                .await
            }
        })),
    )
    .await
    .expect("agents queued behind a process-wide worker cap");

    for out in results {
        assert_eq!(out, "probe-result");
    }
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        AGENTS,
        "all {AGENTS} agents must have their call in flight at once"
    );
}

/// One call straight through `run_single_tool`, with the budget handed in, so a
/// test can spend a small allowance instead of the production default.
async fn call_with_budget(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    budget: Arc<TurnBudget>,
    limits: BudgetLimits,
    name: &str,
    label: &str,
) -> String {
    let call = ToolCallInfo {
        call_id: CallId::new(format!(
            "call-{name}-{label}-{}",
            BUDGET_CALL_SEQ.fetch_add(1, Ordering::SeqCst)
        )),
        name: name.to_string(),
        input: json!({"label": label, "intent": "budget"}),
        parse_err: None,
        item_seq: 900,
        title: format!("{name} test"),
        intent: Some("budget".to_string()),
    };
    let registry = agent
        .tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .snapshot();
    let outcome = run_single_tool(
        session.clone(),
        agent.clone(),
        CancellationToken::new(),
        registry,
        budget,
        limits,
        call,
    )
    .await;
    match outcome.result.content.first() {
        Some(block) => match block {
            ContentBlock::ToolResult { content, .. } => content.clone(),
            other => panic!("unexpected tool result block: {other:?}"),
        },
        None => panic!("a tool call must always answer with a result"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tool_call_allowance_refuses_the_call_past_it() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, session, agent) = test_session(
        Config::default(),
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );
    // Every test in this file shares a session id and a main agent, so a
    // budget test names its own turn to get counters nobody else touched.
    agent.set_current_turn(&pacode_types::TurnId::new("turn-budget-tools"));
    let budget = turn_budget(&session, &agent);
    let limits = BudgetLimits {
        tool_calls: 2,
        exec_calls: 0,
    };

    for label in ["one", "two"] {
        let out = call_with_budget(&session, &agent, budget.clone(), limits, "read", label).await;
        assert_eq!(out, format!("{label}-result"));
    }
    for _ in 0..2 {
        let out = call_with_budget(&session, &agent, budget.clone(), limits, "read", "over").await;
        assert!(
            out.contains("per-turn tool call budget of 2 is spent"),
            "{out}"
        );
        assert!(out.contains("do not retry it"), "{out}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a refused call must never reach the tool"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exec_allowance_only_charges_exec_calls() {
    let (_temp, session, agent) = test_session(Config::default(), Arc::new(ExecTool));
    agent.set_current_turn(&pacode_types::TurnId::new("turn-budget-exec"));
    let budget = turn_budget(&session, &agent);
    let limits = BudgetLimits {
        tool_calls: 0,
        exec_calls: 1,
    };

    assert_eq!(
        call_with_budget(&session, &agent, budget.clone(), limits, "bash", "one").await,
        "ran"
    );
    let out = call_with_budget(&session, &agent, budget.clone(), limits, "bash", "two").await;
    assert!(out.contains("per-turn exec budget of 1 is spent"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zero_allowance_means_no_allowance_at_all() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, session, agent) = test_session(
        Config::default(),
        Arc::new(TestTool {
            calls: calls.clone(),
        }),
    );
    agent.set_current_turn(&pacode_types::TurnId::new("turn-budget-unlimited"));
    let budget = turn_budget(&session, &agent);

    for index in 0..5 {
        let out = call_with_budget(
            &session,
            &agent,
            budget.clone(),
            BudgetLimits {
                tool_calls: 0,
                exec_calls: 0,
            },
            "read",
            &format!("free{index}"),
        )
        .await;
        assert!(out.ends_with("-result"), "{out}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_spending_counters_belong_to_one_turn_only() {
    let (_temp, session, agent) = test_session(Config::default(), Arc::new(ExecTool));

    agent.set_current_turn(&pacode_types::TurnId::new("turn-1"));
    let first = turn_budget(&session, &agent);
    let again = turn_budget(&session, &agent);
    assert!(
        Arc::ptr_eq(&first, &again),
        "one turn must keep spending against the same counters"
    );

    agent.set_current_turn(&pacode_types::TurnId::new("turn-2"));
    let next = turn_budget(&session, &agent);
    assert!(
        !Arc::ptr_eq(&first, &next),
        "a new turn must start from a clean allowance"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_production_allowances_are_the_documented_ones() {
    let (_temp, session, _agent) = test_session(Config::default(), Arc::new(ExecTool));
    let limits = super::turn_budget_limits(&session);
    assert_eq!(limits.tool_calls, 1024);
    assert_eq!(limits.exec_calls, 128);
}

// ---------------------------------------------------------------------------
// One tool result, two consumers: the model is bounded, the hooks are not.
// ---------------------------------------------------------------------------

mod consumers {
    use super::*;

    /// A result over the hard cap whose three landmarks sit in the three regions
    /// a head+tail cut distinguishes: `BEGIN` in the head, `CUTAWAY` in the part
    /// the cap drops, `TAIL` in the tail.
    ///
    /// The marker is placed on the 2/3 boundary because that is where the cut
    /// lands. Anywhere else and it proves nothing: a marker in the head or the
    /// tail survives a correct cut, and one past the end was never inside the
    /// payload. `h` and `t` fill the two regions the cut keeps, so the same
    /// string also asserts that both ends survive.
    fn oversized() -> String {
        const MARKER: &str = "CUTAWAY";
        let total = MAX_TOOL_OUTPUT_BYTES + 4_096;
        let marker_at = total * 2 / 3;
        let mut out = String::with_capacity(total);
        out.push_str("BEGIN");
        out.push_str(&"h".repeat(marker_at - out.len() - MARKER.len()));
        out.push_str(MARKER);
        out.push_str(&"t".repeat(total - out.len() - "TAIL".len()));
        out.push_str("TAIL");
        out
    }

    fn hook_content(payload: &serde_json::Value) -> &str {
        payload["content"].as_str().expect("content is a string")
    }

    #[test]
    fn the_hook_sees_the_whole_result_and_the_model_the_slice() {
        let full = oversized();
        let (for_model, payload) = split_result_for_consumers(full.clone(), false);

        // The hook gets the truth: every byte, including the stretch the model
        // will never see.
        assert_eq!(hook_content(&payload), full, "the hook must not be capped");
        assert!(
            hook_content(&payload).contains("CUTAWAY"),
            "the part the model cannot see is exactly what the hook is for"
        );

        // The model gets a bounded head+tail slice, and is told it is one.
        assert!(
            for_model.len() <= MAX_TOOL_OUTPUT_BYTES,
            "model copy is {} bytes, cap is {MAX_TOOL_OUTPUT_BYTES}",
            for_model.len()
        );
        assert!(for_model.contains("output truncated"), "needs a marker");
        assert!(
            !for_model.contains("CUTAWAY"),
            "the dropped stretch must actually be gone, or the cut is not happening"
        );
        assert!(
            for_model.starts_with("BEGIN") && for_model.ends_with("TAIL"),
            "a head+tail cut keeps both ends: the model needs the end of the output \
             as much as its start, and dropping it would hide why the call finished"
        );
    }

    #[test]
    fn the_hook_is_told_where_the_models_copy_was_cut() {
        let (_, payload) = split_result_for_consumers(oversized(), false);
        assert_eq!(
            payload["truncated_for_model_at_bytes"],
            json!(MAX_TOOL_OUTPUT_BYTES),
            "a hook that cannot see a cut cannot tell it missed data"
        );
    }

    #[test]
    fn an_uncut_result_carries_no_truncation_note() {
        let (_, payload) = split_result_for_consumers("small result".to_string(), true);
        assert_eq!(hook_content(&payload), "small result");
        assert_eq!(payload["is_error"], json!(true));
        assert!(
            payload.get("truncated_for_model_at_bytes").is_none(),
            "no cut happened, so claiming one would make the note a lie"
        );
    }

    #[test]
    fn the_uncut_result_reaches_the_model_unchanged() {
        let (for_model, _) = split_result_for_consumers("exact".to_string(), false);
        assert_eq!(for_model, "exact", "an under-cap result is not a slice");
    }

    #[test]
    fn is_error_survives_the_split() {
        let (_, payload) = split_result_for_consumers(oversized(), true);
        assert_eq!(
            payload["is_error"],
            json!(true),
            "a failed oversized call is precisely what a post_tool_use hook greps for"
        );
    }
}
