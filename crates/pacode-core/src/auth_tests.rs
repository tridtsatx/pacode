use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacode_auth::store::{Account, AuthStore};
use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_store::Store;
use pacode_tools::ToolRegistry;
use pacode_types::protocol::{AuthState, Event, LoginStage, Reply};
use pacode_types::{Attach, Config, ProviderConfig, ProviderDefaults, ProviderKind};

use crate::auth::{
    AuthDriver, LoginOutcome, Progress, ProgressCallback, build_auth_status, get_provider_auth_info,
};
use crate::{Core, CoreDeps};

#[test]
fn test_list_auth_shapes_reply_from_fixture_store() {
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let mut store = AuthStore::new_empty(&auth_path);

    // Active, valid account
    store.upsert(
        "openai",
        Account {
            label: "openai-main".to_string(),
            kind: "oauth".to_string(),
            access: "openai-access-token".to_string(),
            refresh: Some("refresh-tok".to_string()),
            expires_at: Some((pacode_types::now_ms() / 1000) as i64 + 3600),
            email: Some("user@example.com".to_string()),
            extra: serde_json::Map::new(),
        },
    );

    // Account needing attention (expired and no refresh token)
    store.upsert(
        "kimi-code",
        Account {
            label: "kimi-expired".to_string(),
            kind: "oauth".to_string(),
            access: "kimi-access-token".to_string(),
            refresh: None,
            expires_at: Some(1000), // Far in the past
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    // Account needing attention (empty access token)
    store.upsert(
        "devin",
        Account {
            label: "devin-empty".to_string(),
            kind: "oauth".to_string(),
            access: "   ".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );

    let mut config_providers = BTreeMap::new();
    config_providers.insert(
        "openrouter".to_string(),
        ProviderConfig {
            kind: ProviderKind::OpenAi,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: Some("sk-or-test".to_string()),
            ..ProviderConfig::default()
        },
    );

    let auth_infos = build_auth_status(&store, &config_providers);

    // 1. Check openai: Configured, OAuth, accounts list, active set
    let openai = auth_infos
        .iter()
        .find(|i| i.id == "openai")
        .expect("openai exists");
    assert_eq!(openai.display_name, "OpenAI");
    assert_eq!(openai.auth_kind, "oauth");
    assert!(openai.recommended);
    assert_eq!(openai.state, AuthState::Configured);
    assert_eq!(openai.accounts, vec!["openai-main"]);
    assert_eq!(openai.active.as_deref(), Some("openai-main"));

    // 2. Check kimi-code: NeedsAttention due to expired token
    let kimi = auth_infos
        .iter()
        .find(|i| i.id == "kimi-code")
        .expect("kimi-code exists");
    match &kimi.state {
        AuthState::NeedsAttention { reason } => {
            assert!(reason.contains("expired"));
        }
        other => panic!("expected NeedsAttention, got {other:?}"),
    }

    // 3. Check devin: NeedsAttention due to empty credentials
    let devin = auth_infos
        .iter()
        .find(|i| i.id == "devin")
        .expect("devin exists");
    match &devin.state {
        AuthState::NeedsAttention { reason } => {
            assert!(reason.contains("empty credentials"));
        }
        other => panic!("expected NeedsAttention, got {other:?}"),
    }

    // 4. Check openrouter: Configured via config api_key
    let openrouter = auth_infos
        .iter()
        .find(|i| i.id == "openrouter")
        .expect("openrouter exists");
    assert_eq!(openrouter.state, AuthState::Configured);

    // 5. Check ollama: Local endpoint is always Configured
    let ollama = auth_infos
        .iter()
        .find(|i| i.id == "ollama")
        .expect("ollama exists");
    assert_eq!(ollama.auth_kind, "local");
    assert_eq!(ollama.state, AuthState::Configured);

    // 6. Check custom / unconfigured provider: NotConfigured
    let custom = auth_infos
        .iter()
        .find(|i| i.id == "custom")
        .expect("custom exists");
    assert_eq!(custom.state, AuthState::NotConfigured);

    // 7. Single lookup helper
    let single = get_provider_auth_info(&store, &config_providers, "openai");
    assert!(single.is_some());
    assert_eq!(single.unwrap().id, "openai");
}

struct FakeAuthDriver {
    fail: bool,
}

#[async_trait::async_trait]
impl AuthDriver for FakeAuthDriver {
    async fn login(
        &self,
        provider: &str,
        on_progress: ProgressCallback,
    ) -> Result<LoginOutcome, String> {
        on_progress(Progress::OpenUrl {
            url: format!("https://{provider}.example.com/oauth"),
            opened: true,
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        on_progress(Progress::Waiting);
        tokio::time::sleep(Duration::from_millis(10)).await;
        on_progress(Progress::Exchanging);
        tokio::time::sleep(Duration::from_millis(10)).await;

        if self.fail {
            return Err("authentication failed by provider".to_string());
        }

        Ok(LoginOutcome {
            provider: provider.to_string(),
            account: Account {
                label: format!("{provider}-new"),
                kind: "oauth".to_string(),
                access: "token-new-12345".to_string(),
                refresh: Some("refresh-new".to_string()),
                expires_at: Some((pacode_types::now_ms() / 1000) as i64 + 3600),
                email: Some("test@example.com".to_string()),
                extra: serde_json::Map::new(),
            },
        })
    }

    async fn access_token(&self, _provider: &str) -> Result<String, String> {
        Ok("fake-access-token".to_string())
    }
}

async fn build_test_core(paths: Paths) -> Arc<Core> {
    build_test_core_with(paths, pacode_provider::MockProvider::new("mock")).await
}

async fn build_test_core_with(paths: Paths, mock: pacode_provider::MockProvider) -> Arc<Core> {
    paths.ensure_dirs().unwrap();
    let config = Arc::new(Config {
        provider: ProviderDefaults::default(),
        ..Config::default()
    });
    let mut reg = ProviderRegistry::empty();
    reg.insert(Arc::new(mock));
    reg.set_default_route(Some(pacode_types::ModelRoute::new("mock", "mock-model")));
    let providers = Arc::new(reg);
    let tools = ToolRegistry::new();
    let tasks = TaskManager::new(paths.spool_dir(), config.exec.clone());
    let mcp = McpPool::new(BTreeMap::new(), Some(paths.mcp_cache_dir()), None);
    let ui_sink = Arc::new(FakeUiSink);
    let plugins = Arc::new(pacode_plugin::PluginHost::load(&config.plugins, ui_sink).await);
    let marketplace = Arc::new(pacode_plugin::marketplace::Marketplace::new(
        Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
        paths.cache_dir.join("marketplace"),
        pacode_plugin::PluginHost::install_dir(&config.plugins),
        3600,
    ));
    let store = Store::open(&paths.db_file()).unwrap();
    let skills = Arc::new(pacode_skills::SkillRegistry::default());

    let deps = CoreDeps {
        config,
        paths,
        providers,
        tools,
        tasks,
        mcp,
        plugins,
        marketplace,
        store,
        app_version: "0.1.3".to_string(),
        skills,
    };

    Core::new(deps).await
}

struct FakeUiSink;
impl pacode_plugin::UiSink for FakeUiSink {
    fn toast(&self, _text: &str) {}
    fn status(&self, _text: &str) {}
}

#[tokio::test]
async fn test_login_emits_progress_events_in_order_and_persists_on_success() {
    let temp_dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp_dir.path());

    let core = build_test_core(paths).await;
    core.set_auth_driver(Arc::new(FakeAuthDriver { fail: false }));

    // A session is opened only to give the login something to rebuild against; the
    // events themselves arrive on the daemon-wide channel.
    let _session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(pacode_types::ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .expect("open session");

    // Login progress is a daemon-wide event: a client sees it without a session.
    let mut event_rx = core.subscribe_global();

    // Capture broadcast events in background
    let received_events = Arc::new(Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&received_events);
    let listen_handle = tokio::spawn(async move {
        while let Ok((_seq, event)) = event_rx.recv().await {
            let should_break = matches!(
                event,
                Event::LoginProgress {
                    stage: LoginStage::Done { .. } | LoginStage::Failed { .. },
                    ..
                }
            );
            rec_clone.lock().unwrap().push(event);
            if should_break {
                break;
            }
        }
    });

    // Request login
    let reply = core.handle_login("openai".to_string());
    assert_eq!(reply, Reply::Ok);

    // Wait for the login flow task to finish
    let _ = tokio::time::timeout(Duration::from_secs(3), listen_handle).await;

    let events = received_events.lock().unwrap().clone();

    // Verify progress events in order: OpenUrl -> Waiting -> Exchanging -> AuthUpdated -> Done
    let mut stages = Vec::new();
    let mut got_auth_updated = false;

    for ev in events {
        match ev {
            Event::LoginProgress { provider, stage } => {
                assert_eq!(provider, "openai");
                stages.push(stage);
            }
            Event::AuthUpdated(info) => {
                assert_eq!(info.id, "openai");
                assert_eq!(info.active.as_deref(), Some("openai-new"));
                got_auth_updated = true;
            }
            _ => {}
        }
    }

    assert_eq!(stages.len(), 4);
    match &stages[0] {
        LoginStage::OpenUrl { url, opened } => {
            assert!(url.contains("openai.example.com"));
            assert!(*opened);
        }
        other => panic!("expected OpenUrl, got {other:?}"),
    }
    assert_eq!(stages[1], LoginStage::Waiting);
    assert_eq!(stages[2], LoginStage::Exchanging);
    assert_eq!(
        stages[3],
        LoginStage::Done {
            label: "openai-new".to_string()
        }
    );
    assert!(got_auth_updated);

    // Verify account was persisted in the store
    let store = core.load_auth_store();
    let acc = store.get("openai").expect("openai persisted in store");
    assert_eq!(acc.label, "openai-new");
    assert_eq!(acc.access, "token-new-12345");

    // Verify provider was rebuilt in registry
    let reg = core.providers();
    assert!(reg.get("openai").is_some());
}

#[tokio::test]
async fn test_login_failure_emits_failed_stage() {
    let temp_dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp_dir.path());

    let core = build_test_core(paths).await;
    core.set_auth_driver(Arc::new(FakeAuthDriver { fail: true }));

    let _session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(pacode_types::ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .expect("open session");

    // Login progress is a daemon-wide event: a client sees it without a session.
    let mut event_rx = core.subscribe_global();
    let received_events = Arc::new(Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&received_events);

    let listen_handle = tokio::spawn(async move {
        while let Ok((_seq, event)) = event_rx.recv().await {
            let should_break = matches!(
                event,
                Event::LoginProgress {
                    stage: LoginStage::Failed { .. },
                    ..
                }
            );
            rec_clone.lock().unwrap().push(event);
            if should_break {
                break;
            }
        }
    });

    let reply = core.handle_login("openai".to_string());
    assert_eq!(reply, Reply::Ok);

    let _ = tokio::time::timeout(Duration::from_secs(3), listen_handle).await;
    let events = received_events.lock().unwrap().clone();

    let last_stage = events
        .into_iter()
        .filter_map(|ev| match ev {
            Event::LoginProgress { stage, .. } => Some(stage),
            _ => None,
        })
        .next_back()
        .expect("received progress stage");

    match last_stage {
        LoginStage::Failed { message } => {
            assert!(message.contains("authentication failed"));
        }
        other => panic!("expected Failed stage, got {other:?}"),
    }
}

#[tokio::test]
async fn test_logout_and_set_auth_account() {
    let temp_dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp_dir.path());

    let core = build_test_core(paths).await;

    // Seed store with two accounts
    let mut store = core.load_auth_store();
    store.upsert(
        "openai",
        Account {
            label: "openai-1".to_string(),
            kind: "oauth".to_string(),
            access: "tok-1".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.upsert(
        "openai",
        Account {
            label: "openai-2".to_string(),
            kind: "oauth".to_string(),
            access: "tok-2".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.save().unwrap();

    // 1. Set active account to openai-2
    let reply = core.handle_set_auth_account("openai".to_string(), "openai-2".to_string());
    match reply {
        Reply::AuthStatus { providers } => {
            let openai = providers.iter().find(|p| p.id == "openai").unwrap();
            assert_eq!(openai.active.as_deref(), Some("openai-2"));
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }

    // 2. Logout one account (openai-1)
    let reply = core.handle_logout("openai".to_string(), Some("openai-1".to_string()));
    match reply {
        Reply::AuthStatus { providers } => {
            let openai = providers.iter().find(|p| p.id == "openai").unwrap();
            assert_eq!(openai.accounts, vec!["openai-2"]);
            assert_eq!(openai.active.as_deref(), Some("openai-2"));
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }

    // 3. Logout all accounts for openai
    let reply = core.handle_logout("openai".to_string(), None);
    match reply {
        Reply::AuthStatus { providers } => {
            let openai = providers.iter().find(|p| p.id == "openai").unwrap();
            assert!(openai.accounts.is_empty());
            assert_eq!(openai.active, None);
            assert_eq!(openai.state, AuthState::NotConfigured);
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }
}

/// A logout (like a login or an account switch) changes which providers serve
/// models, so attached clients get the new catalog without asking again —
/// that is what unlocks or locks a group in an open model palette.
#[tokio::test]
async fn test_auth_change_pushes_the_catalog_to_attached_sessions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let core = build_test_core(Paths::under(temp_dir.path())).await;
    let session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(pacode_types::ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .expect("open session");
    let mut events = core.subscribe(&session_id).expect("session events");

    let _ = core.handle_logout("claude".to_string(), None);

    let models_updated = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok((_, Event::ModelsUpdated { .. })) => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(models_updated, "a logout must push ModelsUpdated");
}

/// The rebuild after a sign-in swaps the credential, not the cached catalog:
/// the announcement fetches that provider's catalog again, so a fresh TTL on
/// the old account's list cannot ship it to the clients.
#[tokio::test]
async fn test_the_announced_catalog_is_fetched_again_not_served_from_cache() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fresh = pacode_types::ModelInfo {
        route: pacode_types::ModelRoute::new("mock", "new-account-model"),
        display_name: "New".to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
        ..pacode_types::ModelInfo::default()
    };
    let mock = pacode_provider::MockProvider::new("mock");
    mock.set_models(vec![fresh.clone()]);
    let core = build_test_core_with(Paths::under(temp_dir.path()), mock).await;
    let stale = pacode_types::ModelInfo {
        route: pacode_types::ModelRoute::new("mock", "old-account-model"),
        ..fresh.clone()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    core.providers()
        .catalog_cache()
        .seed_memory_cache("mock", now, vec![stale]);
    let session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(pacode_types::ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .expect("open session");
    let mut events = core.subscribe(&session_id).expect("session events");

    core.announce_catalog("mock");

    let models = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok((_, Event::ModelsUpdated { models })) => return Some(models),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("ModelsUpdated");
    assert_eq!(models, vec![fresh]);
}
