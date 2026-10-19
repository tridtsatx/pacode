use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::{TaskManager, TaskSpec};
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, ToolRegistry};
use pacode_types::{
    AgentId, Attach, CallId, Config, ContentBlock, CronSchedule, ModelRoute, PermissionRequest,
    Reply, Request, Role, SessionId,
};
use tokio::sync::Notify;

use crate::core::{Core, CoreDeps, LeaseKind, SessionLease, SessionLeases};

struct GateTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Tool for GateTool {
    fn name(&self) -> &'static str {
        "gate"
    }

    fn description(&self) -> &'static str {
        "Blocks until the test opens the gate"
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
        self.entered.notify_waiters();
        self.release.notified().await;
        Ok(ToolOutput::text("gate opened"))
    }
}

async fn build_test_core(
    temp_dir: &tempfile::TempDir,
    mock_provider: Arc<MockProvider>,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
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

    let mut providers = ProviderRegistry::empty();
    providers.insert(mock_provider);
    providers.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());
    let paths = Paths::under(temp_dir.path());

    let mut tools = ToolRegistry::new();
    if let Some((entered, release)) = gate {
        tools.register(Arc::new(GateTool { entered, release }));
    }

    let deps = CoreDeps {
        marketplace: std::sync::Arc::new(pacode_plugin::marketplace::Marketplace::new(
            std::sync::Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
            paths.cache_dir.join("marketplace"),
            paths.cache_dir.join("plugins"),
            pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
        )),
        config: Arc::new(config),
        paths,
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp,
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store: Store::open_in_memory().unwrap(),
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };

    Core::new(deps).await
}

struct Harness {
    core: Arc<Core>,
    mock: Arc<MockProvider>,
    tmp: tempfile::TempDir,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Harness {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("mock"));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let core = build_test_core(
            &tmp,
            mock.clone(),
            Some((Arc::clone(&entered), Arc::clone(&release))),
        )
        .await;
        Harness {
            core,
            mock,
            tmp,
            entered,
            release,
        }
    }

    async fn open(&self) -> SessionId {
        self.core
            .open_session(Attach::New {
                cwd: self.tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            })
            .await
            .unwrap()
    }

    async fn open_gated(&self) -> SessionId {
        self.mock.push(MockResponse::ToolCalls {
            text: None,
            calls: vec![("gate".to_string(), serde_json::json!({}))],
        });
        self.mock
            .push(MockResponse::Text("after the gate".to_string()));
        self.open().await
    }

    async fn attach(&self, id: &SessionId) -> SessionLease {
        self.core.attach_lease(id).await.unwrap()
    }

    async fn open_attached(&self) -> (SessionId, SessionLease) {
        self.core
            .open_attached(Attach::New {
                cwd: self.tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            })
            .await
            .unwrap()
    }

    async fn wait_for_gate(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .expect("the gated tool never ran");
    }

    async fn spawn_task(&self, id: &SessionId, command: &str) -> pacode_types::TaskId {
        let session = self.core.session(id).unwrap();
        let task = session
            .tasks
            .spawn(TaskSpec::new(
                id.clone(),
                AgentId::main(),
                command.to_string(),
                self.tmp.path(),
            ))
            .await
            .unwrap();
        wait_until(|| self.task_running(id)).await;
        task.id
    }

    fn task_running(&self, id: &SessionId) -> bool {
        self.core.session(id).is_some_and(|session| {
            session
                .tasks_of_session()
                .iter()
                .any(|info| info.status == pacode_types::TaskStatus::Running)
        })
    }
}

async fn wait_until(f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within 5s");
}

async fn wait_unloaded(core: &Core, id: &SessionId) {
    wait_until(|| core.session(id).is_none()).await;
}

async fn assert_unloads_when_idle(core: &Core, id: &SessionId) {
    if core.session(id).is_none() {
        return;
    }
    assert!(
        core.evict_if_idle(id).await,
        "a detached idle session must leave memory"
    );
}

fn permission() -> PermissionRequest {
    PermissionRequest {
        id: pacode_types::PermissionId::generate(),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: CallId::generate(),
        tool: "write".into(),
        title: "Write owned.txt".into(),
        detail: "content".into(),
        risk: None,
        created_at_ms: pacode_types::now_ms(),
    }
}

fn seed_history(agent: &crate::agent::Agent, count: usize) {
    let mut history = agent.history.lock().unwrap();
    for index in 0..count {
        history.push(pacode_types::Message::user(format!("note {index}")));
        history.push(pacode_types::Message::new(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: format!("answer {index}"),
            }],
        ));
    }
}

#[test]
fn a_release_only_drops_the_lease_it_names() {
    let leases = SessionLeases::default();
    let session = SessionId::new("lease-generations");
    let other = SessionId::new("lease-other");

    let first = leases.acquire(&session, LeaseKind::Attach);
    let second = leases.acquire(&session, LeaseKind::Attach);
    let request = leases.acquire(&session, LeaseKind::Request);
    let foreign = leases.acquire(&other, LeaseKind::Attach);

    assert!(leases.holds(&session));
    assert!(!leases.release(&request));
    assert!(!leases.is_candidate(&session));
    assert!(!leases.release(&first));
    assert!(!leases.is_candidate(&session));
    assert_eq!(leases.count(&session, LeaseKind::Attach), 1);

    assert!(!leases.release(&request));
    assert!(leases.release(&second));
    assert!(!leases.holds(&session));
    assert!(leases.is_candidate(&session));

    let reattached = leases.acquire(&session, LeaseKind::Attach);
    assert!(!leases.is_candidate(&session));
    assert!(leases.release(&reattached));
    assert!(leases.is_candidate(&session));
    assert!(!leases.release(&reattached));

    assert!(leases.holds(&other));
    assert!(leases.release(&foreign));
    assert!(leases.is_candidate(&other));
}

#[tokio::test]
async fn an_unleased_session_without_a_detached_attachment_stays() {
    let h = Harness::new().await;
    let session_id = h.open().await;

    assert!(!h.core.is_eviction_candidate(&session_id));
    assert!(!h.core.evict_if_idle(&session_id).await);
    assert!(h.core.session(&session_id).is_some());
}

#[tokio::test]
async fn a_lease_keeps_an_idle_session_until_it_is_released() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let lease = h.attach(&session_id).await;
    assert_eq!(h.core.lease_count(&session_id, LeaseKind::Attach), 1);

    assert!(!h.core.evict_if_idle(&session_id).await);
    assert!(h.core.session(&session_id).is_some());

    h.core.release_lease(&lease);
    assert!(h.core.evict_if_idle(&session_id).await);
    assert!(h.core.session(&session_id).is_none());
    assert!(!h.core.is_leased(&session_id));
    assert_eq!(h.core.lease_count(&session_id, LeaseKind::Attach), 0);
    assert!(
        h.core
            .deps
            .store
            .get_session(&session_id)
            .await
            .unwrap()
            .is_some(),
        "the unloaded session must still exist in the store"
    );
}

#[tokio::test]
async fn a_request_lease_pins_a_detached_session() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let session = h.core.session(&session_id).unwrap();
    seed_history(&session.main_agent().unwrap(), 12);
    let lease = h.attach(&session_id).await;
    h.mock.push(MockResponse::Slow {
        delay: Duration::from_secs(1),
        text: "summary of the session".into(),
    });

    let pending = {
        let core = Arc::clone(&h.core);
        let session = session_id.clone();
        tokio::spawn(async move { core.handle(&session, Request::Compact).await })
    };
    wait_until(|| h.core.lease_count(&session_id, LeaseKind::Request) == 1).await;
    h.core.release_lease(&lease);
    assert!(h.core.is_leased(&session_id));
    assert!(!h.core.is_eviction_candidate(&session_id));
    assert!(!session.has_live_work());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        h.core.session(&session_id).is_some(),
        "an in-flight request must pin its session"
    );

    assert_eq!(pending.await.unwrap(), Reply::Ok);
    assert_eq!(h.core.lease_count(&session_id, LeaseKind::Request), 0);
    assert_unloads_when_idle(&h.core, &session_id).await;
}

#[tokio::test]
async fn a_running_turn_keeps_a_detached_session_until_it_ends() {
    let h = Harness::new().await;
    let session_id = h.open_gated().await;
    let session = h.core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let lease = h.attach(&session_id).await;

    session
        .submit_user_message("hold the gate".into())
        .await
        .unwrap();
    h.wait_for_gate().await;
    h.core.release_lease(&lease);
    assert!(h.core.is_eviction_candidate(&session_id));
    assert!(!h.core.evict_if_idle(&session_id).await);

    h.release.notify_one();
    wait_unloaded(&h.core, &session_id).await;
    assert!(!main.is_running());
}

#[tokio::test]
async fn a_lease_holds_a_session_through_its_turn() {
    let h = Harness::new().await;
    let session_id = h.open_gated().await;
    let lease = h.attach(&session_id).await;
    let session = h.core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();

    session
        .submit_user_message("hold the gate".into())
        .await
        .unwrap();
    h.wait_for_gate().await;
    h.release.notify_one();
    wait_until(|| !main.is_running()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.core.session(&session_id).is_some());

    h.core.release_lease(&lease);
    wait_unloaded(&h.core, &session_id).await;
}

#[tokio::test]
async fn a_pending_ask_keeps_a_detached_session_in_memory() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let session = h.core.session(&session_id).unwrap();

    let request = permission();
    let _waiter = session
        .permissions
        .register(request.clone())
        .expect("permission should register");
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    assert!(!h.core.evict_if_idle(&session_id).await);

    assert!(
        session
            .permissions
            .resolve(&request.id, pacode_types::PermissionDecision::Deny, None)
    );
    assert_unloads_when_idle(&h.core, &session_id).await;
}

#[tokio::test]
async fn a_pending_question_keeps_a_detached_session_in_memory() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let session = h.core.session(&session_id).unwrap();

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
    let _waiter = session.questions.register(question.clone()).unwrap();
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    assert!(!h.core.evict_if_idle(&session_id).await);

    assert!(
        session
            .questions
            .resolve(&question.id, pacode_types::QuestionAnswer::cancelled())
            .is_some()
    );
    assert_unloads_when_idle(&h.core, &session_id).await;
}

#[tokio::test]
async fn an_enabled_cron_job_keeps_a_detached_session_in_memory() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let job = match h
        .core
        .handle(
            &session_id,
            Request::AddCronJob {
                name: "nightly".into(),
                schedule: CronSchedule::parse("0 0 * * *").unwrap(),
                prompt: "summarize".into(),
            },
        )
        .await
    {
        Reply::CronJobs { jobs } => jobs.into_iter().next().unwrap(),
        other => panic!("expected cron jobs, got {other:?}"),
    };
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    assert!(!h.core.evict_if_idle(&session_id).await);

    h.core
        .handle(
            &session_id,
            Request::SetCronEnabled {
                id: job.id.clone(),
                enabled: false,
            },
        )
        .await;
    assert_unloads_when_idle(&h.core, &session_id).await;
}

#[tokio::test]
async fn a_live_monitor_keeps_a_detached_session_in_memory() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let session = h.core.session(&session_id).unwrap();
    let monitor = session
        .scheduler
        .add_monitor(
            "watch".into(),
            pacode_types::MonitorCondition::FileExists {
                path: h.tmp.path().join("later").display().to_string(),
            },
            None,
            pacode_types::now_ms(),
        )
        .unwrap();
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    assert!(!h.core.evict_if_idle(&session_id).await);

    session.scheduler.stop_monitor(&monitor.id).unwrap();
    wait_unloaded(&h.core, &session_id).await;
}

#[tokio::test]
async fn a_running_task_keeps_a_detached_session_in_memory() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    h.spawn_task(&session_id, "sleep 30").await;
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    assert!(!h.core.evict_if_idle(&session_id).await);

    h.core
        .handle(
            &session_id,
            Request::KillTask(
                h.core.session(&session_id).unwrap().tasks_of_session()[0]
                    .id
                    .clone(),
            ),
        )
        .await;
    assert_unloads_when_idle(&h.core, &session_id).await;
}

#[tokio::test]
async fn an_attach_sweeps_a_candidate_nobody_looked_at() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    let lease = h.core.leases.acquire(&session_id, LeaseKind::Attach);
    assert!(h.core.leases.release(&lease));
    assert!(h.core.is_eviction_candidate(&session_id));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.core.session(&session_id).is_some());

    let (current, lease) = h.open_attached().await;
    wait_unloaded(&h.core, &session_id).await;
    assert!(h.core.session(&current).is_some());
    h.core.release_lease(&lease);
}

#[tokio::test]
async fn a_resume_rebuilds_an_unloaded_session() {
    let h = Harness::new().await;
    let session_id = h.open_gated().await;
    let session = h.core.session(&session_id).unwrap();
    let main = session.main_agent().unwrap();
    let lease = h.attach(&session_id).await;
    session
        .submit_user_message("first question".into())
        .await
        .unwrap();
    h.wait_for_gate().await;
    h.release.notify_one();
    wait_until(|| !main.is_running()).await;
    h.core.release_lease(&lease);
    assert_unloads_when_idle(&h.core, &session_id).await;

    let (resumed, lease) = h
        .core
        .open_attached(Attach::Resume {
            session: session_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(resumed, session_id);
    let reloaded = h.core.session(&session_id).unwrap();
    assert!(!Arc::ptr_eq(&reloaded, &session));
    let history = reloaded
        .main_agent()
        .unwrap()
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .messages
        .clone();
    assert!(
        history
            .iter()
            .any(|message| message.text().contains("first question")),
        "the resumed session must keep its history"
    );
    assert!(!h.core.evict_if_idle(&session_id).await);
    h.core.release_lease(&lease);
    assert!(h.core.evict_if_idle(&session_id).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reattach_never_lands_on_an_unloaded_session() {
    let h = Arc::new(Harness::new().await);
    let session_id = h.open().await;
    h.spawn_task(&session_id, "sleep 30").await;

    for _ in 0..32 {
        let lease = h.attach(&session_id).await;
        h.core.release_lease(&lease);
        let evictor = {
            let core = Arc::clone(&h.core);
            let session = session_id.clone();
            tokio::spawn(async move { core.evict_if_idle(&session).await })
        };
        let reattach = {
            let core = Arc::clone(&h.core);
            let session = session_id.clone();
            tokio::spawn(async move { core.open_attached(Attach::Resume { session }).await })
        };
        let (resumed, lease) = reattach.await.unwrap().unwrap();
        assert_eq!(resumed, session_id);
        evictor.await.unwrap();
        assert!(
            h.core.session(&session_id).is_some(),
            "a reattached session must be resident"
        );
        assert!(!h.core.evict_if_idle(&session_id).await);
        h.core.release_lease(&lease);
    }

    h.core
        .handle(
            &session_id,
            Request::KillTask(
                h.core.session(&session_id).unwrap().tasks_of_session()[0]
                    .id
                    .clone(),
            ),
        )
        .await;
    wait_unloaded(&h.core, &session_id).await;
}

#[tokio::test]
async fn shutdown_stops_eviction() {
    let h = Harness::new().await;
    let session_id = h.open().await;
    h.spawn_task(&session_id, "sleep 30").await;
    let lease = h.attach(&session_id).await;
    h.core.release_lease(&lease);
    h.core
        .closing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(!h.core.evict_if_idle(&session_id).await);
    assert!(h.core.session(&session_id).is_some());
    let _: SessionLease = h.attach(&session_id).await;
}

// `/clear` drives the same core through the same harness.
#[path = "clear_tests.rs"]
mod clear_tests;
