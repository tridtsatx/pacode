//! MCP tool proxy: one `Tool` per `(server, tool)` from `codeapp-mcp`.

use std::sync::Arc;

use async_trait::async_trait;
use codeapp_mcp::{McpPool, McpToolInfo};
use serde_json::Value;

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub struct McpTool {
    pool: Arc<McpPool>,
    server: String,
    info: McpToolInfo,
    /// `<server>__<tool>`
    name: String,
}

impl McpTool {
    pub fn new(pool: Arc<McpPool>, server: String, info: McpToolInfo) -> Self {
        let name = codeapp_mcp::tool_name(&server, &info.name);
        Self {
            pool,
            server,
            info,
            name,
        }
    }
}

/// Build proxies for every tool of every configured server (lazy servers answer from
/// the schema cache).
pub async fn mcp_tools(pool: Arc<McpPool>) -> Vec<Arc<dyn Tool>> {
    let _ = pool;
    todo!("mcp::mcp_tools")
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.info.description
    }

    fn schema(&self) -> Value {
        self.info.input_schema.clone()
    }

    /// MCP tools are treated as `Network` (no prompt in Build mode, no edit gate); a
    /// future manifest annotation may refine this.
    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    /// Strips `intent`/`accept_large_output` from the input, calls
    /// `pool.call(server, tool, args)`, maps `is_error`, caps output.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx, &self.server);
        todo!("McpTool::call")
    }
}
