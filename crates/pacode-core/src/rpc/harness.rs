//! The end-to-end harness: a real core, store and turn loop driven by a
//! scripted orchestrator and scripted subagents.
//!
//! One provider serves both roles and routes by the request's system prompt
//! (`## Subagent Role` is only in a subagent's), so each role has its own
//! script and the two never steal each other's responses.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_provider::{CompletionRequest, EventStream, Provider, ProviderError};
use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentMessageDirection, AgentReply, AgentStatus, Attach, CallId, Config, ContentBlock,
    Event, ModelInfo, ModelRoute, StopReason, StreamEvent, TranscriptKind, Usage,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::core::{Core, CoreDeps};
use crate::session::Session;

/// What a scripted step answers with.
pub(crate) enum Reply {
    Text(String),
    Tools(Vec<(String, Value)>),
    /// The provider call fails: the turn ends `Failed`.
    Error(String),
}

pub(crate) type Respond = Box<dyn FnOnce(&str) -> Reply + Send>;

/// One scripted model response; `gate` holds the provider call until a permit
/// is added, which is how a test parks an agent mid-request.
pub(crate) struct Step {
    pub(crate) gate: Option<Arc<Semaphore>>,
    pub(crate) respond: Respond,
}

pub(crate) fn text(s: &str) -> Step {
    let s = s.to_string();
    Step {
        gate: None,
        respond: Box::new(move |_| Reply::Text(s)),
    }
}

pub(crate) fn fail(message: &str) -> Step {
    let message = message.to_string();
    Step {
        gate: None,
        respond: Box::new(move |_| Reply::Error(message)),
    }
}

pub(crate) fn tool(name: &str, input: Value) -> Step {
    let name = name.to_string();
    Step {
        gate: None,
        respond: Box::new(move |_| Reply::Tools(vec![(name, input)])),
    }
}

/// `agent send` to the subagent whose tag (`<agent_question`, `<agent_finished`)
/// the request carries.
pub(crate) fn send_to(tag: &'static str, message: &str) -> Step {
    let message = message.to_string();
    Step {
        gate: None,
        respond: Box::new(move |request| {
            let id = tagged_id(request, tag).expect("the request names the subagent");
            Reply::Tools(vec![(
                "agent".into(),
                json!({"action": "send", "agent_id": id, "message": message}),
            )])
        }),
    }
}

pub(crate) fn stop_asker() -> Step {
    Step {
        gate: None,
        respond: Box::new(|request| {
            let id = tagged_id(request, "<agent_question").expect("a question");
            Reply::Tools(vec![(
                "agent".into(),
                json!({"action": "stop", "agent_id": id}),
            )])
        }),
    }
}

pub(crate) fn gated(gate: &Arc<Semaphore>, mut step: Step) -> Step {
    step.gate = Some(gate.clone());
    step
}

pub(crate) fn tagged_id(request: &str, tag: &str) -> Option<String> {
    let start = request.find(tag)? + tag.len();
    let rest = &request[start..];
    let id_start = rest.find("id=\"")? + 4;
    let id_len = rest[id_start..].find('"')?;
    Some(rest[id_start..id_start + id_len].to_string())
}

/// Everything a request shows the model, tool results included.
pub(crate) fn request_text(request: &CompletionRequest) -> String {
    let mut out = String::new();
    for message in &request.messages {
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => out.push_str(text),
                ContentBlock::ToolResult { content, .. } => out.push_str(content),
                ContentBlock::ToolUse { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::Image { .. } => {}
            }
            out.push('\n');
        }
    }
    out
}

#[derive(Default)]
pub(crate) struct RoleProvider {
    main: Mutex<VecDeque<Step>>,
    sub: Mutex<VecDeque<Step>>,
    /// `(is_subagent, everything the request showed)`, in arrival order.
    seen: Mutex<Vec<(bool, String)>>,
}

impl RoleProvider {
    pub(crate) fn script(&self, sub: bool, steps: Vec<Step>) {
        let queue = if sub { &self.sub } else { &self.main };
        queue.lock().unwrap().extend(steps);
    }

    pub(crate) fn seen(&self, sub: bool) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s == sub)
            .map(|(_, t)| t.clone())
            .collect()
    }
}

#[async_trait]
impl Provider for RoleProvider {
    fn id(&self) -> &str {
        "mock"
    }

    async fn complete(&self, request: CompletionRequest) -> Result<EventStream, ProviderError> {
        let sub = request.system_dynamic.contains("## Subagent Role");
        let shown = request_text(&request);
        self.seen.lock().unwrap().push((sub, shown.clone()));
        let step = if sub { &self.sub } else { &self.main }
            .lock()
            .unwrap()
            .pop_front();
        let reply = match step {
            Some(step) => {
                if let Some(gate) = step.gate {
                    let _ = gate.acquire().await.map(|permit| permit.forget());
                }
                (step.respond)(&shown)
            }
            None => Reply::Text("unscripted".into()),
        };
        let mut events = Vec::new();
        let stop = match reply {
            Reply::Error(message) => {
                return Err(ProviderError::Http {
                    status: 500,
                    message,
                });
            }
            Reply::Text(text) => {
                events.push(StreamEvent::TextDelta { text });
                StopReason::EndTurn
            }
            Reply::Tools(calls) => {
                for (index, (name, input)) in calls.into_iter().enumerate() {
                    events.push(StreamEvent::ToolCallStart {
                        index: index as u32,
                        id: CallId::generate(),
                        name,
                    });
                    events.push(StreamEvent::ToolCallArgsDelta {
                        index: index as u32,
                        delta: input.to_string(),
                    });
                }
                StopReason::ToolUse
            }
        };
        events.push(StreamEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: 2,
            ..Usage::default()
        }));
        events.push(StreamEvent::MessageEnd { stop });
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![self.model_info("mock-model")])
    }

    fn model_info(&self, _model: &str) -> ModelInfo {
        ModelInfo {
            route: ModelRoute::new("mock", "mock-model"),
            display_name: "Mock".into(),
            context_window: Some(128_000),
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        }
    }
}

/// `gate`: a tool call that announces itself on `entered` and parks until the
/// test adds a permit to `release` — how a test holds an agent mid-step.
pub(crate) struct GateTool {
    pub(crate) entered: Semaphore,
    pub(crate) release: Semaphore,
}

#[async_trait]
impl pacode_tools::Tool for GateTool {
    fn name(&self) -> &str {
        "gate"
    }

    fn description(&self) -> &str {
        "Parks until the test releases it (test fixture)"
    }

    fn kind(&self) -> pacode_tools::ToolKind {
        pacode_tools::ToolKind::Control
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: Value,
        _ctx: &pacode_tools::ToolCtx,
    ) -> Result<pacode_tools::ToolOutput, pacode_tools::ToolError> {
        self.entered.add_permits(1);
        let _ = self.release.acquire().await.map(|permit| permit.forget());
        Ok(pacode_tools::ToolOutput::text("released"))
    }
}

pub(crate) struct Harness {
    pub(crate) gate: Arc<GateTool>,
    pub(crate) core: Arc<Core>,
    pub(crate) session: Arc<Session>,
    pub(crate) provider: Arc<RoleProvider>,
    pub(crate) events: Arc<Mutex<Vec<Event>>>,
    _tmp: tempfile::TempDir,
}

impl Harness {
    pub(crate) async fn new() -> Self {
        Self::with_config(Config::default()).await
    }

    pub(crate) async fn with_config(mut config: Config) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(RoleProvider::default());
        let mut registry = pacode_provider::ProviderRegistry::empty();
        registry.insert(provider.clone());
        let route = ModelRoute::new("mock", "mock-model");
        registry.set_default_route(Some(route.clone()));
        config.provider.default = Some("mock/mock-model".to_string());
        let paths = pacode_config::Paths::under(tmp.path());
        let exec = pacode_types::ExecConfig {
            yield_after_secs: 1,
            stall_secs: 100,
            max_spool_bytes: 1024 * 1024,
            tail_bytes: 64 * 1024,
            kill_grace_secs: 1,
            max_tasks: 8,
            default_timeout_secs: 10,
        };
        let gate = Arc::new(GateTool {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let tools = ToolRegistry::new()
            .with(gate.clone())
            .with(Arc::new(pacode_tools::builtin::agent::AgentTool::default()))
            .with(Arc::new(
                pacode_tools::builtin::ask_parent::AskParentTool::default(),
            ))
            .with(Arc::new(
                pacode_tools::builtin::report_status::ReportStatusTool::default(),
            ))
            .with(Arc::new(pacode_tools::builtin::plan::PlanTool::default()));
        let deps = CoreDeps {
            marketplace: Arc::new(pacode_plugin::marketplace::Marketplace::new(
                Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
                paths.cache_dir.join("marketplace"),
                paths.cache_dir.join("plugins"),
                pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
            )),
            config: Arc::new(config),
            paths,
            providers: Arc::new(registry),
            tools,
            tasks: pacode_exec::TaskManager::new(tmp.path().to_path_buf(), exec),
            mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
            plugins: Arc::new(pacode_plugin::PluginHost::new()),
            store: pacode_store::Store::open_in_memory().unwrap(),
            app_version: "0.1.0-test".into(),
            skills: Arc::new(pacode_skills::SkillRegistry::default()),
        };
        let core = Core::new(deps).await;
        let id = core
            .open_session(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: Some(route),
                effort: None,
                mode: None,
            })
            .await
            .unwrap();
        let session = core.session(&id).unwrap();
        session.meta.write().unwrap().name = Some("rpc".into());
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut rx = core.subscribe(&id).unwrap();
        let sink = events.clone();
        tokio::spawn(async move {
            while let Ok((_, event)) = rx.recv().await {
                sink.lock().unwrap().push(event);
            }
        });
        Self {
            gate,
            core,
            session,
            provider,
            events,
            _tmp: tmp,
        }
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    pub(crate) fn main(&self) -> Arc<crate::agent::Agent> {
        self.session.main_agent().unwrap()
    }

    /// Wait until the orchestrator is parked inside a `gate` call.
    pub(crate) async fn gate_entered(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(10), self.gate.entered.acquire())
            .await
            .expect("the gate tool was never called")
            .unwrap();
        permit.forget();
    }

    pub(crate) fn sub(&self) -> Arc<crate::agent::Agent> {
        let agents = self.session.agents.read().unwrap();
        let mut subs = agents.values().filter(|a| !a.id().is_main());
        subs.next().cloned().expect("a subagent was spawned")
    }

    pub(crate) fn subs(&self) -> Vec<Arc<crate::agent::Agent>> {
        let agents = self.session.agents.read().unwrap();
        agents
            .values()
            .filter(|a| !a.id().is_main())
            .cloned()
            .collect()
    }

    pub(crate) fn main_idle(&self) -> bool {
        let main = self.session.main_agent().unwrap();
        !main.is_running() && main.injections.is_empty()
    }

    pub(crate) fn main_turns_ended(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::TurnEnded { agent, .. } if agent.is_main()))
            .count()
    }

    /// The last reply recorded on `agent`'s question item, per transcript owner.
    pub(crate) fn question_replies(&self) -> Vec<(AgentId, Option<AgentReply>)> {
        let mut latest: Vec<(AgentId, u64, Option<AgentReply>)> = Vec::new();
        for event in self.events.lock().unwrap().iter() {
            let (Event::ItemAdded(item) | Event::ItemUpdated(item)) = event else {
                continue;
            };
            let TranscriptKind::AgentMessage {
                direction: AgentMessageDirection::Question,
                reply,
                ..
            } = &item.kind
            else {
                continue;
            };
            match latest
                .iter_mut()
                .find(|(owner, seq, _)| *owner == item.agent && *seq == item.seq)
            {
                Some(slot) => slot.2 = reply.clone(),
                None => latest.push((item.agent.clone(), item.seq, reply.clone())),
            }
        }
        latest.into_iter().map(|(a, _, r)| (a, r)).collect()
    }

    pub(crate) fn saw_status(&self, id: &AgentId, status: AgentStatus) -> bool {
        self.events.lock().unwrap().iter().any(
            |e| matches!(e, Event::AgentUpdated(info) if &info.id == id && info.status == status),
        )
    }
}

pub(crate) async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Spawn one subagent and let the orchestrator's first turn end before the
/// subagent's first step runs, so the question always reaches an idle parent.
pub(crate) async fn spawn_parked(h: &Harness, sub_first: Step) -> Arc<Semaphore> {
    let gate = Arc::new(Semaphore::new(0));
    h.provider.script(
        false,
        vec![
            tool(
                "agent",
                json!({"action": "spawn", "name": "tests", "prompt": "work"}),
            ),
            text("spawned"),
        ],
    );
    h.provider.script(true, vec![gated(&gate, sub_first)]);
    h.session.submit_user_message("go".into()).await.unwrap();
    wait_for("the first orchestrator turn", || h.main_turns_ended() >= 1).await;
    gate
}
