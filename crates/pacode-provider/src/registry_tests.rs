use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_types::{Config, ModelInfo, ModelRoute, ProviderConfig};

use crate::registry::ProviderRegistry;
use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[test]
fn test_registry_from_config_empty_base_url() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "empty_prov".to_string(),
        ProviderConfig {
            base_url: "   ".to_string(),
            api_key: None,
            api_key_env: None,
            models: vec![],
            catalog: false,
            context_window: None,
            reasoning: None,
            effort_map: BTreeMap::new(),
            extra_body: None,
            headers: BTreeMap::new(),
        },
    );

    let cfg = Config {
        providers,
        ..Config::default()
    };

    let api_keys = BTreeMap::new();
    let res = ProviderRegistry::from_config(&cfg, &api_keys);
    assert!(res.is_err());
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("expected error"),
    };
    match err {
        ProviderError::Config(msg) => {
            assert!(msg.contains("empty base_url"));
        }
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[test]
fn test_registry_from_config_and_routes() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "openrouter".to_string(),
        ProviderConfig {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: None,
            api_key_env: None,
            models: vec![],
            catalog: false,
            context_window: None,
            reasoning: None,
            effort_map: BTreeMap::new(),
            extra_body: None,
            headers: BTreeMap::new(),
        },
    );

    let cfg = Config {
        providers,
        provider: pacode_types::ProviderDefaults {
            default: Some("openrouter/anthropic/claude-3.5-sonnet".to_string()),
            ..pacode_types::ProviderDefaults::default()
        },
        ..Config::default()
    };

    let mut api_keys = BTreeMap::new();
    api_keys.insert("openrouter".to_string(), Some("sk-or-test".to_string()));

    let reg = ProviderRegistry::from_config(&cfg, &api_keys).expect("valid registry");
    assert_eq!(
        reg.default_route(),
        Some(&ModelRoute::new(
            "openrouter",
            "anthropic/claude-3.5-sonnet"
        ))
    );

    let prov = reg.get("openrouter");
    assert!(prov.is_some());
    assert_eq!(prov.unwrap().id(), "openrouter");

    let parsed = reg.parse_route("meta-llama/llama-3");
    assert_eq!(
        parsed,
        Some(ModelRoute::new("openrouter", "meta-llama/llama-3"))
    );
}

struct FailingProvider;

#[async_trait::async_trait]
impl Provider for FailingProvider {
    fn id(&self) -> &str {
        "failing"
    }
    async fn complete(&self, _req: CompletionRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Transport("down".to_string()))
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::Transport(
            "models endpoint unreachable".to_string(),
        ))
    }
    fn model_info(&self, model: &str) -> ModelInfo {
        ModelInfo {
            route: ModelRoute::new("failing", model),
            display_name: model.to_string(),
            context_window: None,
            supports_reasoning: false,
            pricing: None,
        }
    }
}

struct SuccessProvider;

#[async_trait::async_trait]
impl Provider for SuccessProvider {
    fn id(&self) -> &str {
        "success"
    }
    async fn complete(&self, _req: CompletionRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Config("not used".to_string()))
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![self.model_info("good-model")])
    }
    fn model_info(&self, model: &str) -> ModelInfo {
        ModelInfo {
            route: ModelRoute::new("success", model),
            display_name: "Good Model".to_string(),
            context_window: Some(64_000),
            supports_reasoning: true,
            pricing: None,
        }
    }
}

#[tokio::test]
async fn test_list_all_models_skips_errors() {
    let mut reg = ProviderRegistry::empty();
    reg.insert(Arc::new(FailingProvider));
    reg.insert(Arc::new(SuccessProvider));

    let all = reg.list_all_models().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].route.model, "good-model");
}
