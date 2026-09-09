//! One MCP server process.

use std::time::Duration;

use codeapp_types::McpServerConfig;
use serde_json::Value;

use crate::{McpCallResult, McpError, McpToolInfo};

pub struct McpClient {
    _private: (),
}

impl McpClient {
    /// Spawn the server, run `initialize` + `notifications/initialized`.
    pub async fn start(
        name: &str,
        cfg: &McpServerConfig,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, McpError> {
        let _ = (name, cfg, cwd);
        todo!("McpClient::start")
    }

    pub fn name(&self) -> &str {
        todo!("McpClient::name")
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        todo!("McpClient::list_tools")
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<McpCallResult, McpError> {
        let _ = (tool, args, timeout);
        todo!("McpClient::call_tool")
    }

    pub fn is_alive(&self) -> bool {
        todo!("McpClient::is_alive")
    }

    /// Kill the process and stop the reader task.
    pub async fn shutdown(&self) {
        todo!("McpClient::shutdown")
    }
}
