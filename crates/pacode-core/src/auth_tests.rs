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
        "claude",
        Account {
            label: "claude-main".to_string(),
            kind: "oauth".to_string(),
            access: "claude-access-token".to_string(),
            refresh: Some("refresh-tok".to_string()),
            expires_at: Some((pacode_types::now_ms() / 1000) as i64 + 3600),
            email: Some("user@example.com".to_string()),
            extra: serde_json::Map::new(),
        },
    );

    // Account needing attention (expired and no refresh token)
    store.upsert(
        "openai",
        Account {
            label: "openai-expired".to_string(),
            kind: "oauth".to_string(),
            access: "openai-access-token".to_string(),
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

    // 1. Check claude: Configured, OAuth, accounts list, active set
    let claude = auth_infos
        .iter()
        .find(|i| i.id == "claude")
        .expect("claude exists");
    assert_eq!(claude.display_name, "Claude");
    assert_eq!(claude.auth_kind, "oauth");
    assert!(claude.recommended);
    assert_eq!(claude.state, AuthState::Configured);
    assert_eq!(claude.accounts, vec!["claude-main"]);
    assert_eq!(claude.active.as_deref(), Some("claude-main"));

    // 2. Check openai: NeedsAttention due to expired token
    let openai = auth_infos
        .iter()
        .find(|i| i.id == "openai")
        .expect("openai exists");
    assert_eq!(openai.display_name, "OpenAI");
    assert_eq!(openai.auth_kind, "oauth");
    assert!(openai.recommended);
    match &openai.state {
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
    let single = get_provider_auth_info(&store, &config_providers, "claude");
    assert!(single.is_some());
    assert_eq!(single.unwrap().id, "claude");
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
    paths.ensure_dirs().unwrap();
    let config = Arc::new(Config {
        provider: ProviderDefaults::default(),
        ..Config::default()
    });
    let mut reg = ProviderRegistry::empty();
    let mock = pacode_provider::MockProvider::new("mock");
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
    let reply = core.handle_login("claude".to_string());
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
                assert_eq!(provider, "claude");
                stages.push(stage);
            }
            Event::AuthUpdated(info) => {
                assert_eq!(info.id, "claude");
                assert_eq!(info.active.as_deref(), Some("claude-new"));
                got_auth_updated = true;
            }
            _ => {}
        }
    }

    assert_eq!(stages.len(), 4);
    match &stages[0] {
        LoginStage::OpenUrl { url, opened } => {
            assert!(url.contains("claude.example.com"));
            assert!(*opened);
        }
        other => panic!("expected OpenUrl, got {other:?}"),
    }
    assert_eq!(stages[1], LoginStage::Waiting);
    assert_eq!(stages[2], LoginStage::Exchanging);
    assert_eq!(
        stages[3],
        LoginStage::Done {
            label: "claude-new".to_string()
        }
    );
    assert!(got_auth_updated);

    // Verify account was persisted in the store
    let store = core.load_auth_store();
    let acc = store.get("claude").expect("claude persisted in store");
    assert_eq!(acc.label, "claude-new");
    assert_eq!(acc.access, "token-new-12345");

    // Verify provider was rebuilt in registry
    let reg = core.providers();
    assert!(reg.get("claude").is_some());
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

    let reply = core.handle_login("claude".to_string());
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
        "claude",
        Account {
            label: "claude-1".to_string(),
            kind: "oauth".to_string(),
            access: "tok-1".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.upsert(
        "claude",
        Account {
            label: "claude-2".to_string(),
            kind: "oauth".to_string(),
            access: "tok-2".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.save().unwrap();

    // 1. Set active account to claude-2
    let reply = core.handle_set_auth_account("claude".to_string(), "claude-2".to_string());
    match reply {
        Reply::AuthStatus { providers } => {
            let claude = providers.iter().find(|p| p.id == "claude").unwrap();
            assert_eq!(claude.active.as_deref(), Some("claude-2"));
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }

    // 2. Logout one account (claude-1)
    let reply = core.handle_logout("claude".to_string(), Some("claude-1".to_string()));
    match reply {
        Reply::AuthStatus { providers } => {
            let claude = providers.iter().find(|p| p.id == "claude").unwrap();
            assert_eq!(claude.accounts, vec!["claude-2"]);
            assert_eq!(claude.active.as_deref(), Some("claude-2"));
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }

    // 3. Logout all accounts for claude
    let reply = core.handle_logout("claude".to_string(), None);
    match reply {
        Reply::AuthStatus { providers } => {
            let claude = providers.iter().find(|p| p.id == "claude").unwrap();
            assert!(claude.accounts.is_empty());
            assert_eq!(claude.active, None);
            assert_eq!(claude.state, AuthState::NotConfigured);
        }
        other => panic!("expected AuthStatus, got {other:?}"),
    }
}
