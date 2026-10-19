use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
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

use pacode_types::{AgentId, Event, TurnStop};

use super::start_turn;

/// Next event matching nothing in particular, or panic after 10 s.
async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<(u64, Event)>) -> Event {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Ok((_, ev))) => return ev,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("event stream closed: {e}"),
            Err(_) => panic!("timed out waiting for an event"),
        }
    }
}

async fn wait_not_running(agent: &crate::agent::Agent) {
    for _ in 0..100 {
        if !agent.is_running() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("agent still running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_start_turn_spawns_exactly_one_turn() {
    let (core, mock, _tmp, sid) = setup().await;
    let session = core.session(&sid).unwrap();
    let main = session.main_agent().unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(600),
        text: "working on it".into(),
    });
    main.injections
        .push(crate::inject::Injection::UserSteer("go".into()));
    let mut rx = session.events.subscribe();

    let mut joins = Vec::new();
    for _ in 0..32 {
        let (s, m) = (session.clone(), main.clone());
        joins.push(tokio::spawn(async move { start_turn(s, m) }));
    }
    let mut started = 0;
    for j in joins {
        if j.await.unwrap() {
            started += 1;
        }
    }
    assert_eq!(started, 1, "exactly one start_turn call may win");
    assert!(main.is_running());
    assert!(!core.is_idle(), "a running turn is not idle");

    // Let the turn get going, then interrupt it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(main.is_running());
    session.interrupt();

    let mut turn_started = 0;
    loop {
        match next_event(&mut rx).await {
            Event::TurnStarted { agent, .. } if agent == AgentId::main() => {
                turn_started += 1;
            }
            Event::TurnEnded { agent, stop, .. } if agent == AgentId::main() => {
                assert_eq!(stop, TurnStop::Interrupted);
                break;
            }
            // Events are consumed asynchronously, so `is_running()` cannot be
            // compared against the moment an event was emitted.
            _ => {}
        }
    }
    assert_eq!(turn_started, 1);
    wait_not_running(&main).await;
    assert!(core.is_idle());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_turn_is_noop_while_running_and_works_again_after() {
    let (core, mock, _tmp, sid) = setup().await;
    let session = core.session(&sid).unwrap();
    let main = session.main_agent().unwrap();
    mock.push(MockResponse::Slow {
        delay: Duration::from_millis(500),
        text: "one".into(),
    });
    main.injections
        .push(crate::inject::Injection::UserSteer("go".into()));
    assert!(start_turn(session.clone(), main.clone()));
    assert!(!start_turn(session.clone(), main.clone()));
    session.interrupt();
    wait_not_running(&main).await;
    assert!(start_turn(session.clone(), main.clone()), "free again");
    session.interrupt();
    wait_not_running(&main).await;
}

#[test]
fn retire_agent_caps_tail_and_frees_tools_and_prompt() {
    use pacode_types::{AgentInfo, AgentKind, Effort, ModelRoute, TranscriptItem, TranscriptKind};

    let id = AgentId::new("sub-retire");
    let info = AgentInfo {
        id: id.clone(),
        name: "retire".to_string(),
        kind: AgentKind::Sub,
        status: pacode_types::AgentStatus::Finished,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: Some(1),
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: Some("done".into()),
        error: None,
    };
    let agent = crate::agent::Agent::new(
        id.clone(),
        info,
        crate::agent::History::default(),
        pacode_tools::ToolRegistry::new(),
        100,
        Some("prompt".to_string()),
    );
    {
        let mut t = agent.transcript.lock().unwrap();
        for seq in 0..40u64 {
            t.upsert(TranscriptItem {
                seq,
                agent: id.clone(),
                ts_ms: 0,
                kind: TranscriptKind::Notice {
                    level: pacode_types::ToastLevel::Info,
                    text: format!("n{seq}"),
                },
            });
        }
        assert_eq!(t.tail.len(), 40);
    }
    let before = agent.info();

    super::retire_agent(&agent);

    let t = agent.transcript.lock().unwrap();
    assert_eq!(t.tail.len(), 16);
    assert_eq!(t.tail.front().map(|i| i.seq), Some(24), "newest items kept");
    assert!(t.tool_items.is_empty());
    drop(t);
    assert!(agent.tools.read().unwrap().is_empty());
    assert!(agent.prompt().is_none());
    assert_eq!(agent.info(), before, "info and summary are untouched");
}
