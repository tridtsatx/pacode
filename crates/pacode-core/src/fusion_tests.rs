//! Fusion pipeline tests: the read-only split, the plan artifact, the two
//! failure policies, and the promise that `None` changes nothing.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pacode_provider::{CompletionRequest, EventStream, Provider, ProviderError};
use pacode_tools::{ToolRegistry, builtin};
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, CallId, Config, ContentBlock, Event, Message,
    ModelInfo, ModelRoute, SessionId, SessionMeta, StopReason, StreamEvent, TranscriptItem,
    TranscriptKind, TurnId, TurnStop, Usage,
};

use crate::agent::{Agent, History};
use crate::fusion::{self, FusionPhase};
use crate::session::Session;
use crate::turn;

static SESSION_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// One scripted model step: the events a provider streams for one call.
///
/// No `Result` around the events: a failing provider is scripted by the
/// `fail` flag on the provider, and wrapping each event in a `Result` only
/// forced a `Clone` bound that `ProviderError` cannot satisfy — so a step could
/// not be handed out twice.
type Step = Vec<StreamEvent>;

/// Answers from a script, one entry per call, and remembers every request.
/// The script is what makes the two halves tellable apart.
#[derive(Default)]
struct Script {
    steps: Mutex<Vec<Step>>,
    seen: Mutex<Vec<CompletionRequest>>,
}

impl Script {
    fn with(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn text_step(text: &str) -> Step {
        vec![
            StreamEvent::TextDelta { text: text.into() },
            StreamEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            }),
            StreamEvent::MessageEnd {
                stop: StopReason::EndTurn,
            },
        ]
    }

    /// A step that calls `name` with `input` and then ends the message.
    fn call_step(call_id: &str, name: &str, input: serde_json::Value) -> Step {
        vec![
            StreamEvent::ToolCallStart {
                index: 0,
                id: CallId::new(call_id.to_string()),
                name: name.to_string(),
            },
            StreamEvent::ToolCallArgsDelta {
                index: 0,
                delta: input.to_string(),
            },
            StreamEvent::MessageEnd {
                stop: StopReason::ToolUse,
            },
        ]
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Tool names advertised in request `call`.
    fn tool_names(&self, call: usize) -> Vec<String> {
        self.requests()
            .get(call)
            .map(|request| {
                request
                    .tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

struct ScriptedProvider {
    id: &'static str,
    script: Arc<Script>,
    fail: bool,
    /// Sends tool names upper-cased on the wire, like a provider that renames
    /// tools (Anthropic OAuth).
    renames_tools: bool,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn wire_tool_name<'a>(&self, name: &'a str) -> std::borrow::Cow<'a, str> {
        if self.renames_tools {
            std::borrow::Cow::Owned(name.to_uppercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<EventStream, ProviderError> {
        let index = {
            let mut seen = self
                .script
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let index = seen.len();
            seen.push(request);
            index
        };
        if self.fail {
            return Err(ProviderError::Config(format!(
                "{} is scripted to fail",
                self.id
            )));
        }
        let step = self
            .script
            .steps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(index)
            .cloned()
            .unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(step.into_iter().map(Ok))))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![self.model_info("any")])
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        ModelInfo {
            route: ModelRoute::new(self.id, model),
            display_name: self.id.into(),
            context_window: Some(128_000),
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
            ..ModelInfo::default()
        }
    }
}

/// What each provider in a test session answers with.
#[derive(Default)]
struct Cast {
    planner: Vec<Step>,
    cheap: Vec<Step>,
    primary: Vec<Step>,
    planner_fails: bool,
    cheap_fails: bool,
}

struct Providers {
    planner: Arc<Script>,
    cheap: Arc<Script>,
    primary: Arc<Script>,
}

struct Harness {
    _temp: tempfile::TempDir,
    session: Arc<Session>,
    agent: Arc<Agent>,
}

async fn harness(fusion: Option<(&str, &str)>, cast: Cast) -> (Harness, Providers) {
    let temp = tempfile::tempdir().unwrap();
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let session_id = SessionId::new(format!("fusion-tests-{sequence}"));
    let agent_id = AgentId::main();
    let now = pacode_types::now_ms();
    let primary_route = ModelRoute::new("primary", "primary-model");
    let meta = SessionMeta {
        id: session_id.clone(),
        name: Some("fusion tests".into()),
        cwd: temp.path().to_path_buf(),
        git_branch: None,
        created_at_ms: now,
        updated_at_ms: now,
        model: primary_route.clone(),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: false,
    };
    let info = AgentInfo {
        id: agent_id.clone(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: now,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: primary_route.clone(),
        effort: pacode_types::Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    };

    let tools = ToolRegistry::new()
        .with(Arc::new(builtin::read::ReadTool))
        .with(Arc::new(builtin::edit::EditTool));
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

    let scripts = Providers {
        planner: Script::with(cast.planner),
        cheap: Script::with(cast.cheap),
        primary: Script::with(cast.primary),
    };
    let mut providers = pacode_provider::ProviderRegistry::empty();
    providers.insert(Arc::new(ScriptedProvider {
        id: "planner",
        script: scripts.planner.clone(),
        fail: cast.planner_fails,
        renames_tools: false,
    }));
    providers.insert(Arc::new(ScriptedProvider {
        id: "cheap",
        script: scripts.cheap.clone(),
        fail: cast.cheap_fails,
        renames_tools: true,
    }));
    providers.insert(Arc::new(ScriptedProvider {
        id: "primary",
        script: scripts.primary.clone(),
        fail: false,
        renames_tools: false,
    }));
    providers.set_default_route(Some(primary_route));

    let mut config = Config::default();
    config.provider.default = Some("primary/primary-model".into());
    config.ui.fusion = fusion.map(
        |(planner, implementer)| pacode_types::config::FusionConfig {
            planner: planner.to_string(),
            implementer: implementer.to_string(),
        },
    );

    let store = pacode_store::Store::open_in_memory().unwrap();
    store.upsert_session(&meta).await.unwrap();
    let session = Arc::new(Session {
        id: session_id,
        meta: std::sync::RwLock::new(meta),
        agents: std::sync::RwLock::new(agents),
        agent_aliases: std::sync::Mutex::new(HashMap::new()),
        plan: std::sync::RwLock::new(Default::default()),
        usage: std::sync::RwLock::new(Default::default()),
        permissions: crate::permissions::PermissionState::default(),
        questions: crate::questions::QuestionState::default(),
        events: crate::transcript::EventSink::new(),
        tasks: pacode_exec::TaskManager::new(
            temp.path().join("spool"),
            pacode_types::ExecConfig::default(),
        ),
        accounting: std::sync::Arc::new(crate::analyze::SessionAccounting::new()),
        store,
        submit_lock: tokio::sync::Mutex::new(()),
        closed: std::sync::atomic::AtomicBool::new(false),
        recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
        config: Arc::new(config),
        providers: Arc::new(providers),
        tools: std::sync::RwLock::new(tools),
        mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        app_version: "test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
        scheduler: crate::schedule::Scheduler::new(),
        model_flags: std::sync::RwLock::new(HashMap::new()),
        global_events: crate::transcript::EventSink::new(),
        idle_watch: crate::core::IdleWatch::default(),
        ledger: crate::ledger::FileLedger::new(),
        efforts_cache: Default::default(),
    });
    (
        Harness {
            _temp: temp,
            session,
            agent,
        },
        scripts,
    )
}

const PAIR: (&str, &str) = ("planner/expensive-model", "cheap/cheap-model");

/// One turn, started the way a submit starts one.
async fn run_turn(harness: &Harness, user: &str) -> TurnStop {
    submit(harness, user).await;
    let mut events = harness.session.events.subscribe();
    turn::start_turn(harness.session.clone(), harness.agent.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok((_, Event::TurnStarted { turn, .. })) => {
                    return wait_for_end(&mut events, &turn).await;
                }
                Ok(_) => {}
                Err(error) => panic!("event stream closed: {error}"),
            }
        }
    })
    .await
    .expect("turn did not end")
}

async fn wait_for_end(
    events: &mut tokio::sync::broadcast::Receiver<(u64, Event)>,
    turn: &TurnId,
) -> TurnStop {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok((
                    _,
                    Event::TurnEnded {
                        turn: ended, stop, ..
                    },
                )) if ended == *turn => return stop,
                Ok(_) => {}
                Err(error) => panic!("event stream closed: {error}"),
            }
        }
    })
    .await
    .expect("turn did not end")
}

/// The two writes a submit makes before the turn starts: the persisted message
/// and the transcript item that rebuilds to it.
async fn submit(harness: &Harness, text: &str) {
    turn::persist_history_message(&harness.session, &harness.agent, Message::user(text))
        .await
        .expect("the store takes a message");
    let seq = harness
        .agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_seq();
    let item = TranscriptItem {
        seq,
        agent: harness.agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::User {
            text: text.to_string(),
        },
    };
    harness
        .agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upsert(item.clone());
    harness.session.events.emit(Event::ItemAdded(item));
}

fn history_texts(agent: &Agent) -> Vec<String> {
    agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .messages
        .iter()
        .map(|message| message.text())
        .collect()
}

/// The names of every tool call in the agent's history, in order. A tool call is
/// a call, not a word: history also holds prose, and prose mentioning a tool is
/// not that tool being called.
fn history_tool_calls(agent: &Agent) -> Vec<String> {
    agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .messages
        .iter()
        .flat_map(|message| message.tool_uses())
        .map(|(_, name, _)| name.to_string())
        .collect()
}

/// The live transcript, in the order a client saw it.
fn live_kinds(agent: &Agent) -> Vec<TranscriptKind> {
    agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .tail
        .iter()
        .map(|item| item.kind.clone())
        .collect()
}

/// The transcript a resumed session rebuilds from the store: the same list,
/// derived from the messages instead of from the live tail.
fn resumed_kinds(agent: &Agent) -> Vec<TranscriptKind> {
    let rows = agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .rows();
    crate::transcript::history_to_items(&agent.id(), &rows)
        .into_iter()
        .map(|item| item.item.kind)
        .collect()
}

/// `(content, is_error)` of every tool result in a request.
fn request_tool_results(request: &CompletionRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn planner_request_carries_only_the_read_only_allowlist() {
    let (harness, scripts) = harness(
        Some(PAIR),
        Cast {
            planner: vec![Script::text_step("1. edit src/lib.rs")],
            cheap: vec![Script::text_step("done")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(
        run_turn(&harness, "rename the parser").await,
        TurnStop::Completed
    );

    let requests = scripts.planner.requests();
    assert_eq!(requests.len(), 1, "the planner runs once per turn");
    assert_eq!(requests[0].model, "expensive-model");
    // The allowlist, not a kind filter: `edit` is registered on this agent, and
    // it is still absent from the planner's request.
    assert_eq!(scripts.planner.tool_names(0), vec!["read".to_string()]);
    assert!(
        scripts.cheap.tool_names(0).contains(&"edit".to_string()),
        "the implementer keeps the agent's own tools"
    );
    // The `## Tools` inventory follows each request's own schemas, named
    // the way that request's provider sends them on the wire.
    assert!(requests[0].system_dynamic.contains("\n- read: "));
    assert!(!requests[0].system_dynamic.contains("\n- edit: "));
    let implementer = &scripts.cheap.requests()[0].system_dynamic;
    assert!(implementer.contains("\n- EDIT: "), "{implementer}");
    assert!(!implementer.contains("\n- edit: "), "{implementer}");
}

#[tokio::test]
async fn planner_edit_call_is_refused_and_the_turn_continues() {
    let (harness, scripts) = harness(
        Some(PAIR),
        Cast {
            planner: vec![
                Script::call_step("call-1", "edit", serde_json::json!({"path": "src/lib.rs"})),
                Script::text_step("plan: change the parser"),
            ],
            cheap: vec![Script::text_step("applied")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(
        run_turn(&harness, "fix the parser").await,
        TurnStop::Completed
    );

    let requests = scripts.planner.requests();
    assert_eq!(requests.len(), 2, "a refusal does not end the planner turn");
    let results = request_tool_results(&requests[1]);
    assert_eq!(results.len(), 1);
    assert!(results[0].1, "a refusal is an error result");
    assert!(
        results[0].0.contains("not available in the planning phase"),
        "the refusal names the rule: {}",
        results[0].0
    );
    // The refused *call*, not the word "edit": the plan preamble legitimately
    // says "do not ask the planner to make the edits", so a substring check over
    // every message would flag the plan for containing the rule that keeps the
    // planner from editing.
    let planner_calls = history_tool_calls(&harness.agent);
    assert!(
        !planner_calls.iter().any(|name| name == "edit"),
        "the planner's own calls never reach the session: {planner_calls:?}"
    );
    let texts = history_texts(&harness.agent);
    assert!(
        texts.iter().any(|text| text.contains("change the parser")),
        "and the plan it went on to write does: {texts:?}"
    );
}

#[tokio::test]
async fn a_fusion_turn_resumes_and_replays_identically() {
    let (harness, scripts) = harness(
        Some(PAIR),
        Cast {
            planner: vec![Script::text_step("change the parser in src/lib.rs")],
            cheap: vec![Script::text_step("applied")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(
        run_turn(&harness, "fix the parser").await,
        TurnStop::Completed
    );

    let plan = history_texts(&harness.agent)
        .into_iter()
        .find(|text| text.contains("change the parser"))
        .expect("the plan is in history");
    assert!(plan.contains("planner/expensive-model"), "plan: {plan}");

    // The resumability claim in one assertion: what the client saw live is
    // what a reopened session rebuilds, item for item, plan step included.
    assert_eq!(live_kinds(&harness.agent), resumed_kinds(&harness.agent));
    assert!(
        live_kinds(&harness.agent).iter().any(
            |kind| matches!(kind, TranscriptKind::User { text } if text.contains("change the parser"))
        ),
        "the plan is a step of its own, not folded into another message"
    );

    // And the implementer was handed the plan as its brief.
    assert!(
        scripts.cheap.requests()[0]
            .messages
            .iter()
            .any(|message| message.text().contains("change the parser"))
    );
}

#[tokio::test]
async fn planner_failure_degrades_to_a_single_model_turn() {
    let (harness, scripts) = harness(
        Some(PAIR),
        Cast {
            // A planner with nothing scripted returns an empty message: no
            // plan, no error. It is still a planner that failed.
            planner: Vec::new(),
            primary: vec![Script::text_step("answered by the primary")],
            ..Cast::default()
        },
    )
    .await;
    let stop = run_turn(&harness, "fix the parser").await;
    assert_eq!(stop, TurnStop::Completed);
    assert_eq!(scripts.planner.requests().len(), 1);
    assert!(
        scripts.cheap.requests().is_empty(),
        "the implementer route is not used without a plan"
    );
    assert_eq!(scripts.primary.requests().len(), 1);
    assert_eq!(scripts.primary.requests()[0].model, "primary-model");

    let texts = history_texts(&harness.agent);
    assert!(
        !texts.iter().any(|text| text.contains("<plan")),
        "a failed planner leaves no half-written plan: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("answered by the primary")),
        "the turn itself still happened: {texts:?}"
    );
    assert_eq!(
        harness.agent.fusion_trace().map(|trace| trace.phase),
        Some(FusionPhase::PlannerFailed)
    );
}

#[tokio::test]
async fn a_failing_planner_degrades_the_same_way() {
    let (harness, scripts) = harness(
        Some(PAIR),
        Cast {
            planner_fails: true,
            primary: vec![Script::text_step("answered anyway")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(
        run_turn(&harness, "fix the parser").await,
        TurnStop::Completed
    );
    assert_eq!(scripts.planner.requests().len(), 1);
    assert_eq!(scripts.primary.requests().len(), 1);
    assert!(
        !history_texts(&harness.agent)
            .iter()
            .any(|text| text.contains("<plan"))
    );
}

#[tokio::test]
async fn implementer_failure_keeps_the_plan_and_reports_it() {
    let (harness, _scripts) = harness(
        Some(PAIR),
        Cast {
            planner: vec![Script::text_step("plan: wire the new flag")],
            cheap_fails: true,
            ..Cast::default()
        },
    )
    .await;
    let stop = run_turn(&harness, "add the flag").await;
    let TurnStop::Failed { message } = stop else {
        panic!("a failed implementer must not report success: {stop:?}");
    };
    assert!(
        message.contains("cheap/cheap-model"),
        "the failure names the half that failed: {message}"
    );

    // The plan was paid for and is what the user has; it stays.
    let texts = history_texts(&harness.agent);
    assert!(
        texts.iter().any(|text| text.contains("wire the new flag")),
        "the plan survives the implementer: {texts:?}"
    );
    assert!(
        live_kinds(&harness.agent).iter().any(
            |kind| matches!(kind, TranscriptKind::Notice { text, .. } if text.contains("failed"))
        ),
        "and the failure is on the transcript, not only in the return value"
    );
}

#[tokio::test]
async fn a_fusion_turn_attributes_both_halves() {
    let (harness, _scripts) = harness(
        Some(PAIR),
        Cast {
            planner: vec![Script::text_step("plan: one line")],
            cheap: vec![Script::text_step("done")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(run_turn(&harness, "go").await, TurnStop::Completed);
    let trace = harness.agent.fusion_trace().expect("traceable");
    assert_eq!(trace.phase, FusionPhase::Complete);
    assert_eq!(trace.planner, ModelRoute::new("planner", "expensive-model"));
    assert_eq!(trace.implementer, ModelRoute::new("cheap", "cheap-model"));
    assert!(trace.planner_usage.output_tokens > 0);
    assert!(trace.implementer_usage.output_tokens > 0);
}

#[tokio::test]
async fn fusion_off_sends_the_plain_single_model_request() {
    let (harness, scripts) = harness(
        None,
        Cast {
            primary: vec![Script::text_step("plain answer")],
            ..Cast::default()
        },
    )
    .await;
    assert_eq!(run_turn(&harness, "hello").await, TurnStop::Completed);
    assert!(harness.agent.fusion_trace().is_none());
    assert!(scripts.planner.requests().is_empty());
    assert_eq!(scripts.primary.requests().len(), 1);
    let request = &scripts.primary.requests()[0];
    assert_eq!(request.model, "primary-model");
    assert!(
        request.tools.iter().any(|tool| tool.name == "edit"),
        "the agent's own tool set, unchanged"
    );
    // No message, no item, no trace: the off path is the pre-fusion path, and
    // the transcript is exactly the two items a plain turn produces.
    assert_eq!(live_kinds(&harness.agent), resumed_kinds(&harness.agent));
    assert_eq!(live_kinds(&harness.agent).len(), 2, "user + answer");
    assert!(
        !history_texts(&harness.agent)
            .iter()
            .any(|text| text.contains("<plan"))
    );
}

#[tokio::test]
async fn a_subagent_never_runs_the_pipeline() {
    let (harness, _scripts) = harness(Some(PAIR), Cast::default()).await;
    assert!(matches!(
        fusion::start_phase(&harness.session, &harness.agent),
        fusion::Phase::Pending(_)
    ));

    let sub_id = AgentId::generate();
    let sub = Agent::new(
        sub_id.clone(),
        AgentInfo {
            id: sub_id,
            name: "sub".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::Idle,
            activity: None,
            started_at_ms: 0,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("primary", "primary-model"),
            effort: pacode_types::Effort::Medium,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
        History::default(),
        ToolRegistry::new(),
        200,
        None,
    );
    assert!(matches!(
        fusion::start_phase(&harness.session, &sub),
        fusion::Phase::Off
    ));
}

#[tokio::test]
async fn a_pair_of_one_model_is_refused() {
    let (harness, _scripts) = harness(
        Some(("primary/primary-model", "primary/primary-model")),
        Cast::default(),
    )
    .await;
    assert!(matches!(
        fusion::start_phase(&harness.session, &harness.agent),
        fusion::Phase::Off
    ));
}

#[tokio::test]
async fn a_route_that_does_not_parse_never_becomes_a_pipeline() {
    let (harness, _scripts) =
        harness(Some(("no-slash", "cheap/cheap-model")), Cast::default()).await;
    assert!(matches!(
        fusion::start_phase(&harness.session, &harness.agent),
        fusion::Phase::Off
    ));
}

#[test]
fn the_stored_plan_is_capped_and_named() {
    let route = ModelRoute::new("planner", "expensive-model");
    let message = fusion::plan_message(&route, &"x".repeat(fusion::PLAN_CAP_CHARS * 2));
    assert!(message.contains("planner/expensive-model"));
    assert!(
        message.contains("truncated"),
        "an oversized plan is cut, not stored whole"
    );
    assert!(
        message.len() < fusion::PLAN_CAP_CHARS * 3,
        "the plan is bounded: {}",
        message.len()
    );
}
