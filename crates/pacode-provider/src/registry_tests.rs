use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_auth::store::{Account, AuthStore};
use pacode_types::{Config, ModelInfo, ModelRoute, ProviderConfig, ProviderKind};

use crate::registry::{
    ProviderRegistry, ResolvedCredential, resolve_provider_credential, synthesize_builtin_providers,
};
use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[test]
fn test_registry_from_config_empty_base_url() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "empty_prov".to_string(),
        ProviderConfig {
            kind: Default::default(),
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
            proxy: None,
            ..Default::default()
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
            kind: Default::default(),
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
            proxy: None,
            ..Default::default()
        },
    );

    let cfg = Config {
        providers,
        provider: pacode_types::ProviderDefaults {
            default: Some("openrouter/openai/gpt-5.5".to_string()),
            ..pacode_types::ProviderDefaults::default()
        },
        ..Config::default()
    };

    let mut api_keys = BTreeMap::new();
    api_keys.insert("openrouter".to_string(), Some("sk-or-test".to_string()));

    let reg = ProviderRegistry::from_config(&cfg, &api_keys).expect("valid registry");
    assert_eq!(
        reg.default_route(),
        Some(ModelRoute::new("openrouter", "openai/gpt-5.5"))
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

#[test]
fn test_credential_precedence_config_beats_env_beats_store() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    store.upsert(
        "my_provider",
        Account {
            label: "store-acc".to_string(),
            kind: "oauth".to_string(),
            access: "tok-store-123".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    // 1. Explicit config key beats both env and store
    let pcfg_explicit = ProviderConfig {
        kind: ProviderKind::OpenAi,
        base_url: "https://example.com/v1".to_string(),
        api_key: Some("key-config-explicit".to_string()),
        api_key_env: Some("MY_ENV_KEY".to_string()),
        ..ProviderConfig::default()
    };
    let mut env_keys = BTreeMap::new();
    env_keys.insert("my_provider".to_string(), Some("key-from-env".to_string()));

    let res = resolve_provider_credential("my_provider", &pcfg_explicit, &env_keys, Some(&store));
    assert_eq!(
        res,
        ResolvedCredential::ConfigApiKey("key-config-explicit".to_string())
    );

    // 2. Env key beats store when config api_key is None
    let pcfg_env = ProviderConfig {
        kind: ProviderKind::OpenAi,
        base_url: "https://example.com/v1".to_string(),
        api_key: None,
        api_key_env: Some("MY_ENV_KEY".to_string()),
        ..ProviderConfig::default()
    };
    let res = resolve_provider_credential("my_provider", &pcfg_env, &env_keys, Some(&store));
    assert_eq!(
        res,
        ResolvedCredential::EnvApiKey("key-from-env".to_string())
    );

    // 3. Store is used when neither config api_key nor env is present
    let pcfg_none = ProviderConfig {
        kind: ProviderKind::OpenAi,
        base_url: "https://example.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        ..ProviderConfig::default()
    };
    let empty_env_keys = BTreeMap::new();
    let res = resolve_provider_credential("my_provider", &pcfg_none, &empty_env_keys, Some(&store));
    match res {
        ResolvedCredential::StoreOAuth(acc) => {
            assert_eq!(acc.access, "tok-store-123");
            assert_eq!(acc.label, "store-acc");
        }
        other => panic!("expected StoreOAuth, got {other:?}"),
    }

    // 4. None when store doesn't have it either
    let res_missing =
        resolve_provider_credential("unknown_prov", &pcfg_none, &empty_env_keys, Some(&store));
    assert_eq!(res_missing, ResolvedCredential::None);
}

#[test]
fn test_transport_selection_per_kind() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    store.upsert(
        "moonshot",
        Account {
            label: "moonshot-key".to_string(),
            kind: "api_key".to_string(),
            access: "moonshot-token".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    let mut providers = BTreeMap::new();
    providers.insert(
        "my-openai".to_string(),
        ProviderConfig {
            kind: ProviderKind::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: Some("sk-openai".to_string()),
            ..ProviderConfig::default()
        },
    );
    providers.insert(
        "my-codex".to_string(),
        ProviderConfig {
            kind: ProviderKind::Codex,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: Some("sk-codex".to_string()),
            ..ProviderConfig::default()
        },
    );

    let cfg = Config {
        providers,
        ..Config::default()
    };

    let reg = ProviderRegistry::from_config_with_store(&cfg, &BTreeMap::new(), Some(&store))
        .expect("builds transports");

    assert!(reg.get("my-openai").is_some());
    assert!(reg.get("moonshot").is_some());
    assert!(reg.get("my-codex").is_some());

    // Devin builds like any other transport now that the protocol is implemented.
    let mut devin_providers = BTreeMap::new();
    devin_providers.insert(
        "my-devin".to_string(),
        ProviderConfig {
            kind: ProviderKind::Devin,
            base_url: "https://api.devin.ai".to_string(),
            ..ProviderConfig::default()
        },
    );
    let devin_cfg = Config {
        providers: devin_providers,
        ..Config::default()
    };
    let devin_res =
        ProviderRegistry::from_config_with_store(&devin_cfg, &BTreeMap::new(), Some(&store));
    let devin_reg = devin_res.expect("devin registry builds");
    assert!(
        devin_reg.get("my-devin").is_some(),
        "devin transport is wired"
    );

    // OpenCode builds OpenCodeProvider transport
    let mut opencode_providers = BTreeMap::new();
    opencode_providers.insert(
        "my-opencode".to_string(),
        ProviderConfig {
            kind: ProviderKind::OpenCode,
            base_url: "https://opencode.ai/zen/v1".to_string(),
            ..ProviderConfig::default()
        },
    );
    let opencode_cfg = Config {
        providers: opencode_providers,
        ..Config::default()
    };
    let opencode_reg =
        ProviderRegistry::from_config_with_store(&opencode_cfg, &BTreeMap::new(), None)
            .expect("opencode registry builds");
    assert!(
        opencode_reg.get("my-opencode").is_some(),
        "opencode transport is wired"
    );
}

#[test]
fn test_synthesized_builtin_providers() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    store.upsert(
        "openrouter",
        Account {
            label: "openrouter-1".to_string(),
            kind: "api_key".to_string(),
            access: "sk-or-test".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.upsert(
        "openai",
        Account {
            label: "openai-1".to_string(),
            kind: "oauth".to_string(),
            access: "openai-oauth-token".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    let mut extra = serde_json::Map::new();
    extra.insert(
        "api_server_url".to_string(),
        serde_json::Value::String("https://api.devin.ai/rpc".to_string()),
    );
    store.upsert(
        "devin",
        Account {
            label: "devin-1".to_string(),
            kind: "oauth".to_string(),
            access: "devin-api-key".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra,
        },
    );

    let mut providers = BTreeMap::new();
    synthesize_builtin_providers(&mut providers, &store);

    // openrouter synthesized from its stored key
    let openrouter_cfg = &providers["openrouter"];
    assert_eq!(openrouter_cfg.kind, ProviderKind::OpenAi);
    assert_eq!(openrouter_cfg.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(
        openrouter_cfg.api_key_env.as_deref(),
        Some("OPENROUTER_API_KEY")
    );
    assert!(!providers.contains_key("anthropic"));
    assert!(!providers.contains_key("claude"));

    // openai synthesized
    assert!(providers.contains_key("openai"));
    let openai_cfg = &providers["openai"];
    assert_eq!(openai_cfg.kind, ProviderKind::Codex);
    assert_eq!(openai_cfg.base_url, "https://api.openai.com/v1");

    // devin synthesized
    assert!(providers.contains_key("devin"));
    let devin_cfg = &providers["devin"];
    assert_eq!(devin_cfg.kind, ProviderKind::Devin);
    assert_eq!(devin_cfg.base_url, "https://api.devin.ai/rpc");

    // opencode synthesized (unconditionally, even without store entry)
    assert!(providers.contains_key("opencode"));
    let opencode_cfg = &providers["opencode"];
    assert_eq!(opencode_cfg.kind, ProviderKind::OpenCode);
    assert_eq!(opencode_cfg.base_url, "https://opencode.ai/zen/v1");
    assert_eq!(opencode_cfg.api_key.as_deref(), Some("public"));
    assert!(opencode_cfg.headers.contains_key("User-Agent"));
    assert_eq!(
        opencode_cfg
            .headers
            .get("x-opencode-client")
            .map(String::as_str),
        Some("cli")
    );

    // from_config_with_store with empty config builds synthesized providers
    let empty_cfg = Config::default();
    let reg = ProviderRegistry::from_config_with_store(&empty_cfg, &BTreeMap::new(), Some(&store))
        .expect("builds synthesized registry");

    assert!(reg.get("openrouter").is_some());
    assert!(reg.get("openai").is_some());
    assert!(reg.get("opencode").is_some());
}

#[test]
fn test_devin_api_server_url_from_extra() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    let mut extra = serde_json::Map::new();
    extra.insert(
        "api_server_url".to_string(),
        serde_json::Value::String("https://custom.devin.endpoint.test".to_string()),
    );
    store.upsert(
        "devin",
        Account {
            label: "devin-acc".to_string(),
            kind: "oauth".to_string(),
            access: "devin-key".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra,
        },
    );

    let mut providers = BTreeMap::new();
    synthesize_builtin_providers(&mut providers, &store);

    let devin = providers.get("devin").expect("devin synthesized");
    assert_eq!(devin.base_url, "https://custom.devin.endpoint.test");
    assert_eq!(devin.kind, ProviderKind::Devin);
}

#[tokio::test]
async fn test_resolve_refreshes_token_when_changed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    store.upsert(
        "moonshot",
        Account {
            label: "moonshot-1".to_string(),
            kind: "oauth".to_string(),
            access: "initial-token".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    store
        .save()
        .expect("persist store so a refresh reads the same file");

    let empty_cfg = Config::default();
    let mut reg =
        ProviderRegistry::from_config_with_store(&empty_cfg, &BTreeMap::new(), Some(&store))
            .expect("registry");

    let route = ModelRoute::new("moonshot", "kimi-k3");
    let initial_prov = reg.resolve(&route).await.expect("resolve initial");
    assert_eq!(initial_prov.id(), "moonshot");

    // Register token refresher that produces a new token
    let refreshed_token = Arc::new(std::sync::Mutex::new("refreshed-token-xyz".to_string()));
    let refresher_token_clone = Arc::clone(&refreshed_token);
    reg.set_token_refresher(Arc::new(move |_prov| {
        let tok = refresher_token_clone.lock().unwrap().clone();
        Box::pin(async move { Ok(tok) })
    }));

    let refreshed_prov = reg.resolve(&route).await.expect("resolve refreshed");
    assert_eq!(refreshed_prov.id(), "moonshot");

    // Provider in registry was rebuilt with the fresh token
    let current_token = reg.store_providers.read().unwrap().get("moonshot").cloned();
    assert_eq!(current_token.as_deref(), Some("refreshed-token-xyz"));
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
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
            ..Default::default()
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
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
            ..Default::default()
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

#[test]
fn test_anthropic_kind_is_rejected() {
    let mut providers = BTreeMap::new();
    providers.insert(
        "legacy".to_string(),
        ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: "https://example.test/v1".to_string(),
            api_key: Some("sk-legacy".to_string()),
            ..ProviderConfig::default()
        },
    );
    let cfg = Config {
        providers,
        ..Config::default()
    };
    let res = ProviderRegistry::from_config_with_store(&cfg, &BTreeMap::new(), None);
    match res {
        Err(ProviderError::Config(msg)) => assert!(msg.contains("no longer supported")),
        Err(other) => panic!("expected Config error, got {other:?}"),
        Ok(_) => panic!("anthropic kind must not build"),
    }
}

#[test]
fn test_config_entry_overrides_builtin_and_api_key_login_synthesizes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut store = AuthStore::new_empty(temp_dir.path().join("auth.json"));
    for id in ["zai", "zai-coding", "kimi-code", "opencode-go", "gemini"] {
        store.upsert(
            id,
            Account {
                label: format!("{id}-1"),
                kind: "api_key".to_string(),
                access: "key".to_string(),
                refresh: None,
                expires_at: None,
                email: None,
                extra: serde_json::Map::new(),
            },
        );
    }
    let mut providers = BTreeMap::new();
    providers.insert(
        "zai".to_string(),
        ProviderConfig {
            base_url: "https://proxy.example.test/zai".to_string(),
            ..ProviderConfig::default()
        },
    );
    synthesize_builtin_providers(&mut providers, &store);
    assert_eq!(providers["zai"].base_url, "https://proxy.example.test/zai");
    assert_eq!(
        providers["zai-coding"].base_url,
        "https://api.z.ai/api/coding/paas/v4"
    );
    assert_eq!(
        providers["kimi-code"].base_url,
        "https://api.kimi.com/coding/v1"
    );
    assert_eq!(
        providers["opencode-go"].base_url,
        "https://opencode.ai/zen/go/v1"
    );
    assert!(providers.contains_key("gemini"));
}

#[tokio::test]
async fn test_resolve_does_not_touch_auth_file_when_token_is_fresh() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("auth.json");
    let mut store = AuthStore::new_empty(&path);
    store.upsert(
        "moonshot",
        Account {
            label: "moonshot-1".to_string(),
            kind: "oauth".to_string(),
            access: "tok".to_string(),
            refresh: Some("r".to_string()),
            expires_at: Some(i64::MAX / 4),
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.save().expect("save");

    let reg = ProviderRegistry::from_config_with_store(
        &Config::default(),
        &BTreeMap::new(),
        Some(&store),
    )
    .expect("registry");
    let route = ModelRoute::new("moonshot", "kimi-k3");
    // Remove the file: a fresh cached token must not need it at all.
    let content = std::fs::read_to_string(&path).expect("read");
    std::fs::remove_file(&path).expect("rm");
    for _ in 0..1000 {
        reg.resolve(&route).await.expect("resolve");
    }
    assert!(!path.exists(), "resolve must not rewrite auth.json");
    std::fs::write(&path, content).expect("restore");
}

#[tokio::test]
async fn test_login_rebuild_invalidates_cached_token() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("auth.json");
    let mut store = AuthStore::new_empty(&path);
    let account = |access: &str| Account {
        label: "moonshot-1".to_string(),
        kind: "oauth".to_string(),
        access: access.to_string(),
        refresh: None,
        expires_at: None,
        email: None,
        extra: serde_json::Map::new(),
    };
    store.upsert("moonshot", account("old"));
    store.save().expect("save");
    let reg = ProviderRegistry::from_config_with_store(
        &Config::default(),
        &BTreeMap::new(),
        Some(&store),
    )
    .expect("registry");
    let route = ModelRoute::new("moonshot", "kimi-k3");
    reg.resolve(&route).await.expect("resolve");

    store.upsert("moonshot", account("new"));
    store.save().expect("save");
    reg.rebuild_provider_from_store("moonshot", &store)
        .expect("rebuild");
    reg.resolve(&route).await.expect("resolve");
    let current = reg
        .store_providers
        .read()
        .expect("lock")
        .get("moonshot")
        .cloned();
    assert_eq!(current.as_deref(), Some("new"));
}
