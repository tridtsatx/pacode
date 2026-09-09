use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::{McpPool, SamplingMessage, SamplingRequest};
use pacode_plugin::PluginHost;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::{Config, ModelRoute};

use super::*;
use crate::core::{Core, CoreDeps};

async fn create_test_core(
    sampling_enabled: bool,
    max_tokens: u32,
) -> (Arc<Core>, Arc<MockProvider>) {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let exec_cfg = pacode_types::ExecConfig::default();
    let tasks = TaskManager::new(temp_dir.path().to_path_buf(), exec_cfg);
    let mcp = McpPool::new(BTreeMap::new(), None, None);

    let mock_provider = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock_provider.clone());
    reg.set_default_route(Some(ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    }));

    let mut config = Config::default();
    config.mcp.sampling = sampling_enabled;
    config.mcp.sampling_max_tokens = max_tokens;
    config.provider.default = Some("mock/mock-model".to_string());

    let paths = Paths::under(temp_dir.path());
    let tools = ToolRegistry::new();
    let plugins = Arc::new(PluginHost::new());

    let deps = CoreDeps {
        config: Arc::new(config),
        paths,
        providers: Arc::new(reg),
        tools,
        tasks,
        mcp,
        plugins,
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };

    let core = Core::new(deps).await;
    (core, mock_provider)
}

#[tokio::test]
async fn test_sampling_success() {
    let (core, mock) = create_test_core(true, 100).await;
    mock.push(MockResponse::Text("Sampling answer".to_string()));

    let handler = CoreSamplingHandler::new(Arc::downgrade(&core));
    let req = SamplingRequest {
        messages: vec![SamplingMessage {
            role: "user".to_string(),
            content: serde_json::json!("Hello from MCP"),
        }],
        system_prompt: Some("You are a helpful assistant".to_string()),
        max_tokens: Some(50),
        model_preferences: None,
        include_context: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
    };

    let resp = handler
        .create_message(req)
        .await
        .expect("sampling succeeded");
    assert_eq!(resp.role, "assistant");
    assert_eq!(resp.model, "mock-model");
    assert_eq!(resp.content["text"], "Sampling answer");
    assert_eq!(resp.stop_reason.as_deref(), Some("endTurn"));

    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].max_output_tokens, Some(50));
}

#[tokio::test]
async fn test_sampling_disabled_error() {
    let (core, _mock) = create_test_core(false, 100).await;
    let handler = CoreSamplingHandler::new(Arc::downgrade(&core));
    let req = SamplingRequest {
        messages: vec![SamplingMessage {
            role: "user".to_string(),
            content: serde_json::json!("Hello"),
        }],
        system_prompt: None,
        max_tokens: None,
        model_preferences: None,
        include_context: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
    };

    let err = handler.create_message(req).await.unwrap_err();
    assert!(matches!(err, McpError::Protocol(_)));
}

#[tokio::test]
async fn test_sampling_cap_max_tokens() {
    let (core, mock) = create_test_core(true, 50).await;
    mock.push(MockResponse::Text("Capped answer".to_string()));

    let handler = CoreSamplingHandler::new(Arc::downgrade(&core));
    let req = SamplingRequest {
        messages: vec![SamplingMessage {
            role: "user".to_string(),
            content: serde_json::json!("Hello"),
        }],
        system_prompt: None,
        max_tokens: Some(500), // higher than cap of 50
        model_preferences: None,
        include_context: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
    };

    let _resp = handler.create_message(req).await.unwrap();
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].max_output_tokens, Some(50));
}
