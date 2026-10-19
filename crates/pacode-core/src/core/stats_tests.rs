use std::sync::Arc;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::MockProvider;
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::{Attach, Config, ExecConfig, ModelRoute};

use super::parse_vm_rss_kb;
use crate::core::{Core, CoreDeps};

#[test]
fn rss_is_parsed_from_status_text() {
    let text = "Name:\tpacode\nVmRSS:\t   20480 kB\nThreads:\t4\n";
    assert_eq!(parse_vm_rss_kb(text), 20480);
    assert_eq!(parse_vm_rss_kb("nothing"), 0);
}

#[tokio::test]
async fn debug_stats_is_valid_json_with_all_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = ProviderRegistry::empty();
    reg.insert(Arc::new(MockProvider::new("mock")));
    reg.set_default_route(Some(ModelRoute::new("mock", "mock-model")));
    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());
    let paths = Paths::under(dir.path());
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
        tasks: TaskManager::new(dir.path().to_path_buf(), ExecConfig::default()),
        mcp: McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store: Store::open_in_memory().unwrap(),
        app_version: "test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };
    let core = Core::new(deps).await;
    core.open_session(Attach::New {
        cwd: dir.path().to_path_buf(),
        model: Some(ModelRoute::new("mock", "mock-model")),
        effort: None,
        mode: None,
    })
    .await
    .unwrap();

    let value = core.debug_stats();
    let text = serde_json::to_string(&value).unwrap();
    let back: serde_json::Value = serde_json::from_str(&text).unwrap();
    for key in [
        "sessions",
        "agents",
        "tasks",
        "store_queue_depth",
        "provider_gate",
        "broadcast",
        "process",
    ] {
        assert!(back.get(key).is_some(), "missing key {key}");
    }
    assert_eq!(back["sessions"], 1);
    let agents =
        back["agents"]["live"].as_u64().unwrap() + back["agents"]["finished"].as_u64().unwrap();
    assert_eq!(agents, 1);
    assert_eq!(back["broadcast"].as_array().map(Vec::len), Some(1));
}
