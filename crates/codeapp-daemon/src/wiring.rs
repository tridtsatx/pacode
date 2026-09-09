//! Build the core from config and paths.

use std::sync::Arc;

use codeapp_core::Core;

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
    let _ = opts;
    todo!("wiring::build_core")
}
