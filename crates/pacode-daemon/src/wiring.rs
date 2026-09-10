//! Build the core from config and paths.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_core::{Core, CoreDeps};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_plugin::{PluginHost, UiSink};
use pacode_provider::mock::MockResponse;
use pacode_provider::{MockProvider, ProviderRegistry};
use pacode_store::Store;
use pacode_tools::builtin_tools;
use pacode_types::ModelRoute;

use crate::{DaemonError, DaemonOptions};

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
/// `pacode_config::resolve_api_key`; tools = `pacode_tools::builtin_tools()` +
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

    let (skill_registry, _warnings) = if opts.config.skills.enabled {
        let dirs = if opts.config.skills.dirs.is_empty() {
            vec![opts.paths.skills_dir()]
        } else {
            opts.config.skills.dirs.clone()
        };
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

    let mut tools = builtin_tools();
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
    let mcp = McpPool::new(
        opts.config.mcp.servers.clone(),
        Some(opts.paths.mcp_cache_dir()),
        None,
    );
    mcp.set_idle_timeout_secs(opts.config.mcp.idle_timeout_secs);
    let store = Store::open(&opts.paths.db_file()).map_err(DaemonError::Store)?;

    let deps = CoreDeps {
        config: Arc::clone(&opts.config),
        paths: opts.paths.clone(),
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp: mcp.clone(),
        plugins: plugin_host,
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
