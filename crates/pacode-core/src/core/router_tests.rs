use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::MockProvider;
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::{Attach, Config, ModelRoute};

use crate::core::{Core, CoreDeps};

async fn setup() -> (
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
    let mock = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock.clone());
    reg.set_default_route(Some(ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    }));
    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());
    let paths = Paths::under(temp_dir.path());
    let deps = CoreDeps {
        marketplace: Arc::new(pacode_plugin::marketplace::Marketplace::new(
            Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
            paths.cache_dir.join("marketplace"),
            paths.cache_dir.join("plugins"),
            pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
        )),
        config: Arc::new(config),
        paths,
        providers: Arc::new(reg),
        tools: ToolRegistry::new(),
        tasks,
        mcp,
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };
    let core = Core::new(deps).await;
    let sid = core
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
    (core, mock, temp_dir, sid)
}

use pacode_exec::{TaskEvent, TaskSpec};
use pacode_types::{AgentId, TaskInfo};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::start_task_router_since;
use crate::inject::Injection;

/// Run `true` to completion and return its terminal info.
async fn finished_task(
    core: &Core,
    sid: &pacode_types::SessionId,
    cwd: &std::path::Path,
) -> TaskInfo {
    let session = core.session(sid).unwrap();
    let info = session
        .tasks
        .spawn(TaskSpec::new(sid.clone(), AgentId::main(), "true", cwd))
        .await
        .unwrap();
    for _ in 0..200 {
        if let Some(done) = session
            .tasks
            .list(Some(sid))
            .into_iter()
            .find(|t| t.id == info.id && t.status.is_terminal())
        {
            return done;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("task never finished");
}

/// Core's own router is stopped so only the router under test routes.
fn stop_core_router(core: &Core) {
    if let Some(h) = core.task_router.lock().unwrap().take() {
        h.abort();
    }
}

/// Collect `TaskFinished` ids drained from the main agent for `window`.
async fn finished_ids(agent: &crate::agent::Agent, window: Duration) -> Vec<pacode_types::TaskId> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while tokio::time::Instant::now() < deadline {
        for inj in agent.injections.drain() {
            if let Injection::TaskFinished { task, .. } = inj {
                out.push(task.id);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn router_survives_lag_and_routes_a_later_ended() {
    let (core, _mock, tmp, sid) = setup().await;
    stop_core_router(&core);
    let session = core.session(&sid).unwrap();
    let main = session.main_agent().unwrap();
    // A running turn makes `start_turn` a no-op, so injections stay queued to inspect.
    *main.cancel.lock().unwrap() = Some(CancellationToken::new());

    let first = finished_task(&core, &sid, tmp.path()).await;
    let (tx, rx) = broadcast::channel::<TaskEvent>(4);
    for _ in 0..10 {
        tx.send(TaskEvent::Progress(first.clone())).unwrap();
    }
    // The first recv sees Lagged; the router must carry on.
    let handle = start_task_router_since(Arc::downgrade(&core), rx, 0);
    let got = finished_ids(&main, Duration::from_millis(500)).await;
    assert_eq!(
        got,
        vec![first.id.clone()],
        "reconcile routes the lost Ended once"
    );
    assert!(!handle.is_finished(), "router must survive Lagged");

    let second = finished_task(&core, &sid, tmp.path()).await;
    tx.send(TaskEvent::Ended(second.clone())).unwrap();
    let got = finished_ids(&main, Duration::from_millis(500)).await;
    assert_eq!(got, vec![second.id.clone()]);
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_ended_is_injected_exactly_once_even_if_ended_arrives_later() {
    let (core, _mock, tmp, sid) = setup().await;
    stop_core_router(&core);
    let session = core.session(&sid).unwrap();
    let main = session.main_agent().unwrap();
    *main.cancel.lock().unwrap() = Some(CancellationToken::new());

    let info = finished_task(&core, &sid, tmp.path()).await;
    let (tx, rx) = broadcast::channel::<TaskEvent>(4);
    for _ in 0..10 {
        tx.send(TaskEvent::Progress(info.clone())).unwrap();
    }
    let handle = start_task_router_since(Arc::downgrade(&core), rx, 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The same Ended showing up now must not inject a second time.
    tx.send(TaskEvent::Ended(info.clone())).unwrap();
    let got = finished_ids(&main, Duration::from_millis(500)).await;
    assert_eq!(got, vec![info.id.clone()]);
    handle.abort();
}

#[test]
fn routed_set_is_capped_and_raises_its_floor() {
    let mut set = super::RoutedSet::default();
    for i in 0..(super::ROUTED_CAP as u64 + 10) {
        assert!(set.mark(&pacode_types::TaskId::new(format!("t{i}")), 1000 + i));
    }
    assert_eq!(set.ids.len(), super::ROUTED_CAP);
    assert_eq!(set.floor_ms, 1000 + 9);
    assert!(!set.mark(&pacode_types::TaskId::new("t20"), 1020));
}
