//! C7: a finished turn's usage is persisted, so `usage_totals` survives a restart.

use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_core::core::{Core, CoreDeps};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::{Attach, Config, Event, ModelRoute, Request, TurnStop};

#[tokio::test(flavor = "multi_thread")]
async fn usage_rows_are_written_per_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let tasks = TaskManager::new(tmp.path().to_path_buf(), Default::default());
    let mock = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock.clone());
    reg.set_default_route(Some(ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    }));
    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());
    let paths = Paths::under(tmp.path());
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
        mcp: McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };
    let core = Core::new(deps).await;
    let sid = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let mut rx = core.subscribe(&sid).unwrap();

    for n in 0..2 {
        mock.push(MockResponse::Text(format!("answer {n}")));
        if n == 0 {
            // The session-title request after the first reply consumes one script too.
            mock.push(MockResponse::Text("Usage test".into()));
        }
        core.handle(
            &sid,
            Request::UserMessage {
                text: format!("question {n}"),
                attachments: Vec::new(),
            },
        )
        .await;
        let wait = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok((_, Event::TurnEnded { stop, .. })) = rx.recv().await {
                    assert_eq!(stop, TurnStop::Completed);
                    break;
                }
            }
        });
        wait.await.expect("turn did not finish");
    }

    let session = core.session(&sid).unwrap();
    let live = session.usage.read().unwrap().clone();
    let mut totals = session.store.usage_totals(&sid).await.unwrap();
    for _ in 0..50 {
        if totals.turns == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        totals = session.store.usage_totals(&sid).await.unwrap();
    }
    assert_eq!(totals.turns, 2);
    assert_eq!(totals.input, live.input);
    assert_eq!(totals.output, live.output);
}
