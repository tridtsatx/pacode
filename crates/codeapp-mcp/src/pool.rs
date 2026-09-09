//! Server pool with lazy start and schema cache.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use codeapp_types::McpServerConfig;
use serde_json::Value;

use crate::{CacheDir, McpCallResult, McpError, McpToolInfo};

pub struct McpPool {
    _private: (),
}

impl McpPool {
    pub fn new(
        servers: BTreeMap<String, McpServerConfig>,
        cache_dir: CacheDir,
        cwd: Option<PathBuf>,
    ) -> Arc<Self> {
        let _ = (servers, cache_dir, cwd);
        todo!("McpPool::new")
    }

    pub fn server_names(&self) -> Vec<String> {
        todo!("McpPool::server_names")
    }

    /// Tools of every server: non-lazy servers are started, lazy ones answer from the
    /// schema cache when present (else started once to fill it).
    pub async fn list_all_tools(&self) -> Vec<(String, McpToolInfo)> {
        todo!("McpPool::list_all_tools")
    }

    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpToolInfo>, McpError> {
        let _ = server;
        todo!("McpPool::list_tools")
    }

    /// Start the server if needed and call the tool with the server's `timeout_secs`.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<McpCallResult, McpError> {
        let _ = (server, tool, args);
        todo!("McpPool::call")
    }

    pub async fn shutdown(&self) {
        todo!("McpPool::shutdown")
    }
}
