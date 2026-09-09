//! Build the core from config and paths.

use std::collections::BTreeMap;
use std::sync::Arc;

use codeapp_core::{Core, CoreDeps};
use codeapp_exec::TaskManager;
use codeapp_mcp::McpPool;
use codeapp_provider::mock::MockResponse;
use codeapp_provider::{MockProvider, ProviderRegistry};
use codeapp_store::Store;
use codeapp_tools::builtin_tools;
use codeapp_types::ModelRoute;

use crate::{DaemonError, DaemonOptions};

/// providers = `ProviderRegistry::from_config` with keys from
/// `codeapp_config::resolve_api_key`; tools = `codeapp_tools::builtin_tools()` (MCP
/// tools are added per session by the core from the pool); tasks =
/// `TaskManager::new(paths.spool_dir(), config.exec.clone())`; mcp =
/// `McpPool::new(config.mcp.servers.clone(), Some(paths.mcp_cache_dir()), None)`;
/// store = `Store::open(paths.db_file())`.
///
/// When the environment variable `CODEAPP_MOCK_PROVIDER` is set, a
/// `codeapp_provider::MockProvider` named `mock` is inserted and made the default
/// route (`mock/mock-model`) — used by `codeapp run --json` smoke tests.
pub async fn build_core(opts: &DaemonOptions) -> Result<Arc<Core>, DaemonError> {
    opts.paths.ensure_dirs().map_err(DaemonError::Io)?;

    let mut api_keys = BTreeMap::new();
    for (id, pcfg) in &opts.config.providers {
        let key = codeapp_config::resolve_api_key(pcfg);
        api_keys.insert(id.clone(), key);
    }

    let mut providers =
        ProviderRegistry::from_config(&opts.config, &api_keys).map_err(DaemonError::Provider)?;

    if std::env::var("CODEAPP_MOCK_PROVIDER").is_ok() {
        let mock = MockProvider::new("mock");
        for _ in 0..50 {
            mock.push(MockResponse::Text(
                "Hello from the mock provider.".to_string(),
            ));
        }
        providers.insert(Arc::new(mock));
        providers.set_default_route(Some(ModelRoute::new("mock", "mock-model")));
    }

    let tools = builtin_tools();
    let tasks = TaskManager::new(opts.paths.spool_dir(), opts.config.exec.clone());
    let mcp = McpPool::new(
        opts.config.mcp.servers.clone(),
        Some(opts.paths.mcp_cache_dir()),
        None,
    );
    let store = Store::open(&opts.paths.db_file()).map_err(DaemonError::Store)?;

    let deps = CoreDeps {
        config: Arc::clone(&opts.config),
        paths: opts.paths.clone(),
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp,
        store,
        app_version: opts.app_version.clone(),
    };

    let core = Core::new(deps).await;
    Ok(core)
}
