use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_core::core::{Core, CoreDeps};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_plugin::{NoopUiSink, PluginHost};
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::builtin_tools;
use pacode_types::{Attach, Config, Event, ModelRoute, Reply, Request, ToolStatus, TranscriptKind};

#[tokio::test(flavor = "multi_thread")]
async fn test_lua_plugin_full_flow() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let tasks = TaskManager::new(
        temp_dir.path().to_path_buf(),
        pacode_types::ExecConfig::default(),
    );
    let mcp = McpPool::new(BTreeMap::new(), None, None);

    let mock_provider = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock_provider.clone());
    reg.set_default_route(Some(ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    }));

    let mut config = Config::default();
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    config.plugins.dirs = vec![fixture_dir];

    let paths = Paths::under(temp_dir.path());
    let ui_sink = Arc::new(NoopUiSink);
    let plugin_host = Arc::new(PluginHost::load(&config.plugins, ui_sink).await);

    let mut tools = builtin_tools();
    for tool in pacode_tools::builtin::plugin::plugin_tools(plugin_host.clone()) {
        tools.register(tool);
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
        providers: Arc::new(reg),
        tools,
        tasks,
        mcp,
        plugins: plugin_host.clone(),
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };

    let core = Core::new(deps).await;

    // 1. ListPlugins returns it
    let reply = core
        .handle_global(&Request::ListPlugins)
        .await
        .expect("ListPlugins reply");
    match reply {
        Reply::Plugins { plugins } => {
            let p = plugins
                .iter()
                .find(|p| p.name == "echo-lua")
                .expect("echo-lua plugin present");
            assert_eq!(p.version, "0.1.0");
            assert_eq!(p.kind, "lua");
            assert!(p.tools.contains(&"echo_tool".to_string()));
            assert!(p.commands.contains(&"echo_cmd".to_string()));
        }
        other => panic!("expected Reply::Plugins, got {other:?}"),
    }

    // 2. RunPluginCommand returns InsertText
    let reply = core
        .handle_global(&Request::RunPluginCommand {
            name: "echo_cmd".to_string(),
            args: "world".to_string(),
        })
        .await
        .expect("RunPluginCommand reply");
    match reply {
        Reply::PluginCommand(pacode_types::protocol::PluginCommandOutcome::InsertText { text }) => {
            assert_eq!(text, "echo: world");
        }
        other => panic!("expected PluginCommand InsertText, got {other:?}"),
    }

    // 3. Assert its tool appears in the tool list (session tools)
    let cwd = temp_dir.path().to_path_buf();
    let session_id = core
        .open_session(Attach::New {
            cwd,
            model: Some(ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            }),
            effort: None,
            mode: Some(pacode_types::Mode::Bypass),
        })
        .await
        .unwrap();

    let session = core.session(&session_id).unwrap();
    {
        let session_tools = session.tools.read().unwrap();
        assert!(
            session_tools.get("echo_tool").is_some(),
            "echo_tool should appear in tool list"
        );
    }

    // 4. A pre_tool_call deny blocks a tool
    mock_provider.push(MockResponse::ToolCalls {
        text: None,
        calls: vec![(
            "echo_tool".into(),
            serde_json::json!({"block": true, "msg": "should fail"}),
        )],
    });
    mock_provider.push(MockResponse::Text("After deny".into()));

    let mut rx = core.subscribe(&session_id).unwrap();
    core.handle(
        &session_id,
        Request::UserMessage {
            text: "call tool".into(),
        },
    )
    .await;

    let timeout = tokio::time::sleep(Duration::from_secs(4));
    tokio::pin!(timeout);

    let mut saw_denied = false;
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for events"),
            res = rx.recv() => {
                let (_seq, event) = res.unwrap();
                match event {
                    Event::ItemUpdated(item) => {
                        if let TranscriptKind::ToolCall { status, preview, .. } = item.kind
                            && (status == ToolStatus::Denied || preview == "Denied")
                        {
                            saw_denied = true;
                        }
                    }
                    Event::TurnEnded { .. } => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        saw_denied,
        "tool execution should have been denied by plugin hook"
    );
}
