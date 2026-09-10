//! MCP tool proxy: one `Tool` per `(server, tool)` from `pacode-mcp`, and one
//! `<server>__resource` tool per server that has resources.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use pacode_mcp::{McpPool, McpToolInfo};
use serde_json::Value;

use super::helpers::cap_output;
use crate::{ACCEPT_LARGE_OUTPUT_KEY, INTENT_KEY, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub struct McpTool {
    pool: Arc<McpPool>,
    server: String,
    info: McpToolInfo,
    /// `<server>__<tool>`
    name: String,
    kind: ToolKind,
}

impl McpTool {
    pub fn new(pool: Arc<McpPool>, server: String, info: McpToolInfo) -> Self {
        Self::with_kind(pool, server, info, ToolKind::Exec)
    }

    pub fn with_kind(
        pool: Arc<McpPool>,
        server: String,
        info: McpToolInfo,
        kind: ToolKind,
    ) -> Self {
        let name = pacode_mcp::tool_name(&server, &info.name);
        Self {
            pool,
            server,
            info,
            name,
            kind,
        }
    }
}

pub struct McpResourceTool {
    pool: Arc<McpPool>,
    server: String,
    /// `<server>__resource`
    name: String,
}

impl McpResourceTool {
    pub fn new(pool: Arc<McpPool>, server: String) -> Self {
        let name = pacode_mcp::tool_name(&server, "resource");
        Self { pool, server, name }
    }
}

/// Build proxies for every tool of every configured server (lazy servers answer from
/// the schema cache), alongside a `<server>__resource` proxy for servers with resources.
pub async fn mcp_tools(pool: Arc<McpPool>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    let all_tools = pool.list_all_tools().await;
    for (server, info) in all_tools {
        tools.push(Arc::new(McpTool::new(pool.clone(), server, info)));
    }

    let all_resources = pool.list_all_resources().await;
    let mut servers_with_resources = BTreeSet::new();
    for (server, _) in all_resources {
        servers_with_resources.insert(server);
    }

    for server in servers_with_resources {
        tools.push(Arc::new(McpResourceTool::new(pool.clone(), server)));
    }

    tools
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

    /// MCP tools default to `ToolKind::Exec` (fail closed: requires permission prompt in
    /// Build and Auto modes) unless explicitly configured otherwise.
    fn kind(&self) -> ToolKind {
        self.kind
    }

    /// Strips `intent`/`accept_large_output` from the input, requests permission,
    /// calls `pool.call(server, tool, args)`, maps `is_error`, caps output.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let accept_large_output = input
            .get(ACCEPT_LARGE_OUTPUT_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut args = input;
        if let Value::Object(ref mut map) = args {
            map.remove(INTENT_KEY);
            map.remove(ACCEPT_LARGE_OUTPUT_KEY);
        }

        let title = format!("MCP: {}", self.name);
        let detail = format!(
            "Server: {}\nTool: {}\nArguments: {}",
            self.server,
            self.info.name,
            serde_json::to_string_pretty(&args).unwrap_or_else(|_| args.to_string())
        );
        ctx.require_permission(title, detail, None).await?;

        let result = self
            .pool
            .call(&self.server, &self.info.name, args)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;

        let capped = cap_output(&result.content, accept_large_output, ctx.output_cap_chars);

        let preview = capped
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .to_string();

        let mut output = if result.is_error {
            ToolOutput::error(capped)
        } else {
            ToolOutput::text(capped)
        };

        output = output.with_title(&self.name).with_preview(preview);
        Ok(output)
    }
}

#[async_trait]
impl Tool for McpResourceTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Read an MCP resource by URI from this server"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The URI of the resource to read"
                }
            },
            "required": ["uri"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let accept_large_output = input
            .get(ACCEPT_LARGE_OUTPUT_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let uri = input
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid("missing required 'uri' parameter"))?;

        let content = self
            .pool
            .read_resource(&self.server, uri)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;

        let capped = cap_output(&content, accept_large_output, ctx.output_cap_chars);

        let preview = capped
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .to_string();

        let mut output = ToolOutput::text(capped);
        output = output.with_title(&self.name).with_preview(preview);
        Ok(output)
    }
}
