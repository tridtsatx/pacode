use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn tick(base: Instant, secs: u64) -> Instant {
    base + Duration::from_secs(secs)
}

#[tokio::test]
async fn idle_session_is_evicted_on_second_sighting_and_resumes() {
    let (core, _mock, _tmp, sid) = setup().await;
    let base = Instant::now();
    let now_ms = pacode_types::now_ms();
    let max_idle = Duration::from_secs(600);

    // Not idle long enough by `updated_at`.
    assert_eq!(core.evict_idle_at(max_idle, base, now_ms), 0);
    let later_ms = now_ms + 601_000;
    // First idle sighting only starts the clock.
    assert_eq!(core.evict_idle_at(max_idle, tick(base, 1), later_ms), 0);
    assert!(core.session(&sid).is_some());
    // Seen idle for max_idle: evicted.
    assert_eq!(core.evict_idle_at(max_idle, tick(base, 602), later_ms), 1);
    assert!(core.session(&sid).is_none());
    assert!(core.idle_since.lock().unwrap().is_empty());

    let resumed = core
        .open_session(Attach::Resume {
            session: sid.clone(),
        })
        .await
        .unwrap();
    assert_eq!(resumed, sid);
    let session = core.session(&sid).unwrap();
    assert!(session.agent(&pacode_types::AgentId::main()).is_some());
}

#[tokio::test]
async fn subscribed_session_is_never_evicted() {
    let (core, _mock, _tmp, sid) = setup().await;
    let _rx = core.subscribe(&sid).unwrap();
    let base = Instant::now();
    let later_ms = pacode_types::now_ms() + 601_000;
    let max_idle = Duration::from_secs(600);
    assert_eq!(core.evict_idle_at(max_idle, base, later_ms), 0);
    assert_eq!(core.evict_idle_at(max_idle, tick(base, 700), later_ms), 0);
    assert!(core.session(&sid).is_some());
}

#[tokio::test]
async fn busy_session_is_never_evicted() {
    let (core, _mock, tmp, sid) = setup().await;
    let session = core.session(&sid).unwrap();
    session
        .tasks
        .spawn(pacode_exec::TaskSpec::new(
            sid.clone(),
            pacode_types::AgentId::main(),
            "sleep 30",
            tmp.path(),
        ))
        .await
        .unwrap();
    drop(session);
    let base = Instant::now();
    let later_ms = pacode_types::now_ms() + 601_000;
    let max_idle = Duration::from_secs(600);
    assert_eq!(core.evict_idle_at(max_idle, base, later_ms), 0);
    assert_eq!(core.evict_idle_at(max_idle, tick(base, 700), later_ms), 0);
    assert!(core.session(&sid).is_some());
    core.close_session(&sid).await;
}
