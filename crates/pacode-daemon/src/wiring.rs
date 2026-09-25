//! Build the core from config and paths.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use pacode_core::{Core, CoreDeps};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_plugin::{PluginHost, UiSink};
use pacode_provider::mock::MockResponse;
use pacode_provider::{MockProvider, ProviderRegistry};
use pacode_store::{Store, StoreError};
use pacode_tools::builtin_tools;
use pacode_types::ModelRoute;

use crate::{DaemonError, DaemonOptions};

fn store_registry() -> &'static std::sync::Mutex<HashMap<PathBuf, Store>> {
    static REGISTRY: OnceLock<std::sync::Mutex<HashMap<PathBuf, Store>>> = OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// The one [`Store`] worker per database file in this process. The core and the
/// daemon's attach-admission path both take a handle here instead of opening a
/// second connection to the same file; `release_store` drops the entry once the
/// daemon that owns it is gone.
pub fn shared_store(path: &Path) -> Result<Store, StoreError> {
    let mut registry = store_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(store) = registry.get(path) {
        return Ok(store.clone());
    }
    let store = Store::open(path)?;
    registry.insert(path.to_path_buf(), store.clone());
    Ok(store)
}

pub fn release_store(path: &Path) {
    let mut registry = store_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.remove(path);
}

struct DaemonUiSink {
    core: Arc<std::sync::RwLock<Option<std::sync::Weak<Core>>>>,
}

impl UiSink for DaemonUiSink {
    fn toast(&self, text: &str) {
        if let Ok(guard) = self.core.read()
            && let Some(weak) = guard.as_ref()
            && let Some(core) = weak.upgrade()
        {
            core.broadcast_event(pacode_types::Event::PluginToast {
                plugin: String::new(),
                text: text.to_string(),
            });
        }
    }

    fn status(&self, text: &str) {
        if let Ok(guard) = self.core.read()
            && let Some(weak) = guard.as_ref()
            && let Some(core) = weak.upgrade()
        {
            core.broadcast_event(pacode_types::Event::PluginStatus {
                plugin: String::new(),
                text: text.to_string(),
            });
        }
    }
}

/// providers = `ProviderRegistry::from_config` with keys from
/// `pacode_config::resolve_api_key`; tools = `pacode_tools::builtin_tools(&config.web)` +
/// plugin tools (MCP tools are added per session by the core from the pool);
/// tasks = `TaskManager::new(paths.spool_dir(), config.exec.clone())`; mcp =
/// `McpPool::new(config.mcp.servers.clone(), Some(paths.mcp_cache_dir()), None)`;
/// store = `Store::open(paths.db_file())`.
///
/// When the environment variable `PACODE_MOCK_PROVIDER` is set, a
/// `pacode_provider::MockProvider` named `mock` is inserted and made the default
/// route (`mock/mock-model`) — used by `pacode run --json` smoke tests.
pub async fn build_core(opts: &DaemonOptions) -> Result<Arc<Core>, DaemonError> {
    opts.paths.ensure_dirs().map_err(DaemonError::Io)?;

    // The provider governors take their ceiling from a process-wide atomic that
    // every request reads, so this is the one config limit the daemon applies
    // for real. `pacode-config` clamps the value into the range the runtime
    // accepts on load; the runtime stays the authority on what it takes.
    let concurrency = usize::try_from(opts.config.limits.max_provider_concurrency)
        .unwrap_or(pacode_provider::DEFAULT_PROVIDER_CONCURRENCY);
    if let Err(error) = pacode_provider::set_provider_concurrency_ceiling(concurrency) {
        log::warn!("provider concurrency {concurrency} rejected by the runtime: {error}");
    }

    let mut api_keys = BTreeMap::new();
    for (id, pcfg) in &opts.config.providers {
        let key = pacode_config::resolve_api_key(pcfg);
        api_keys.insert(id.clone(), key);
    }

    let mut providers =
        ProviderRegistry::from_config(&opts.config, &api_keys).map_err(DaemonError::Provider)?;
    providers.set_cache_path(opts.paths.catalog_cache_file());

    if std::env::var("PACODE_MOCK_PROVIDER").is_ok() {
        let mock = MockProvider::new("mock");
        for _ in 0..50 {
            mock.push(MockResponse::Text(
                "Hello from the mock provider.".to_string(),
            ));
        }
        providers.insert(Arc::new(mock));
        providers.set_default_route(Some(ModelRoute::new("mock", "mock-model")));
    }

    let core_slot: Arc<std::sync::RwLock<Option<std::sync::Weak<Core>>>> =
        Arc::new(std::sync::RwLock::new(None));
    let ui_sink = Arc::new(DaemonUiSink {
        core: core_slot.clone(),
    });

    let plugin_host = Arc::new(PluginHost::load(&opts.config.plugins, ui_sink).await);
    // Installs land in the same directory the host scans, so a fetched plugin is
    // picked up like a hand-written one.
    let marketplace = Arc::new(pacode_plugin::marketplace::Marketplace::new(
        Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
        opts.paths.cache_dir.join("marketplace"),
        PluginHost::install_dir(&opts.config.plugins),
        pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
    ));

    let (skill_registry, _warnings) = if opts.config.skills.enabled {
        let mut dirs = if opts.config.skills.dirs.is_empty() {
            vec![opts.paths.skills_dir()]
        } else {
            opts.config.skills.dirs.clone()
        };
        // A plugin installed from a marketplace keeps its skills in the Claude
        // Code layout, which is the one this loader reads: pointing it at those
        // directories is what makes an installed plugin's skills actually work.
        dirs.extend(marketplace.skill_dirs());
        let (reg, warnings) = pacode_skills::SkillRegistry::load(&dirs);
        for warning in &warnings {
            log::warn!("{warning}");
        }
        (Arc::new(reg), warnings)
    } else {
        (
            Arc::new(pacode_skills::SkillRegistry::default()),
            Vec::new(),
        )
    };

    let mut tools = builtin_tools(&opts.config.web);
    // `[tools.<name>] enabled = false` drops the tool from the model schema and
    // refuses later registrations (plugins, skill, per-session MCP tools).
    tools.apply_disabled(opts.config.disabled_tool_names().map(str::to_string));
    for tool in pacode_tools::builtin::plugin::plugin_tools(plugin_host.clone()) {
        tools.register(tool);
    }
    if opts.config.skills.enabled {
        tools.register(Arc::new(pacode_tools::builtin::skill::SkillTool::new(
            skill_registry.clone(),
            opts.config.skills.max_body_bytes,
        )));
    }

    let tasks = TaskManager::new(opts.paths.spool_dir(), opts.config.exec.clone());
    // MCP servers declared by installed plugins join the configured ones under a
    // `<plugin>/<server>` name, so two plugins cannot collide on `github`.
    let mut mcp_servers = opts.config.mcp.servers.clone();
    for (name, decl) in marketplace.mcp_servers() {
        mcp_servers
            .entry(name)
            .or_insert(pacode_types::McpServerConfig {
                command: decl.command,
                args: decl.args,
                env: decl.env,
                url: decl.url,
                headers: decl.headers,
                enabled: true,
                lazy: true,
                timeout_secs: pacode_types::McpServerConfig::default().timeout_secs,
            });
    }

    let mcp = McpPool::new(mcp_servers, Some(opts.paths.mcp_cache_dir()), None);
    mcp.set_idle_timeout_secs(opts.config.mcp.idle_timeout_secs);
    let store = shared_store(&opts.paths.db_file()).map_err(DaemonError::Store)?;

    let deps = CoreDeps {
        config: Arc::clone(&opts.config),
        paths: opts.paths.clone(),
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp: mcp.clone(),
        plugins: plugin_host,
        marketplace,
        store,
        app_version: opts.app_version.clone(),
        skills: skill_registry,
    };

    let core = Core::new(deps).await;
    if let Ok(mut slot) = core_slot.write() {
        *slot = Some(Arc::downgrade(&core));
    }

    let sampling_handler = Arc::new(pacode_core::sampling::CoreSamplingHandler::new(
        Arc::downgrade(&core),
    ));
    mcp.set_sampling_handler(sampling_handler);
    mcp.set_sampling_config(
        opts.config.mcp.sampling,
        opts.config.mcp.sampling_max_tokens,
    );

    Ok(core)
}

#[cfg(test)]
mod store_tests {
    use std::path::Path;

    use pacode_types::SessionId;

    use super::{release_store, shared_store};

    fn open_db_handles(path: &Path) -> Option<usize> {
        let entries = std::fs::read_dir("/proc/self/fd").ok()?;
        let target = std::fs::canonicalize(path).ok()?;
        Some(
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    std::fs::read_link(entry.path())
                        .ok()
                        .and_then(|link| std::fs::canonicalize(link).ok())
                        .is_some_and(|open| open == target)
                })
                .count(),
        )
    }

    #[tokio::test]
    async fn shared_store_is_one_worker_per_database_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shared.sqlite");
        let missing = SessionId::new("missing");

        let first = shared_store(&path).unwrap();
        let Some(opened) = open_db_handles(&path) else {
            return;
        };
        assert_eq!(opened, 1, "shared_store opened {opened} handles");

        let second = shared_store(&path).unwrap();
        assert_eq!(
            open_db_handles(&path),
            Some(opened),
            "a second worker was opened on the same file"
        );
        assert!(first.get_session(&missing).await.is_ok());
        assert!(second.get_session(&missing).await.is_ok());

        drop(first);
        drop(second);
        release_store(&path);
        let reopened = shared_store(&path).unwrap();
        assert!(
            reopened.get_session(&missing).await.is_ok(),
            "a released entry handed out a dead worker"
        );
    }
}
