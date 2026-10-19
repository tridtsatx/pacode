//! MCP tool proxy: one `Tool` per `(server, tool)` from `pacode-mcp`, and one
//! `<server>__resource` tool per server that has resources.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use pacode_mcp::{McpPool, McpToolInfo};
use serde_json::Value;

use super::helpers::{cap_output, preview_body};
use crate::{
    ACCEPT_LARGE_OUTPUT_KEY, DefinitionCache, INTENT_KEY, Tool, ToolCtx, ToolError, ToolKind,
    ToolOutput,
};

pub struct McpTool {
    pool: Arc<McpPool>,
    server: String,
    info: McpToolInfo,
    /// `<server>__<tool>`
    name: String,
    kind: ToolKind,
    /// A listing is immutable for the life of the proxy, so the memoised
    /// definition needs no invalidation: a refreshed pool is served by
    /// [`mcp_tools`], which builds *new* proxies (with new, empty caches) rather
    /// than mutating the ones a session already holds.
    definition: DefinitionCache,
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
            definition: DefinitionCache::new(),
        }
    }
}

pub struct McpResourceTool {
    pool: Arc<McpPool>,
    server: String,
    /// `<server>__resource`
    name: String,
    /// Fixed by construction, so it has no refresh path.
    definition: DefinitionCache,
}

impl McpResourceTool {
    pub fn new(pool: Arc<McpPool>, server: String) -> Self {
        let name = pacode_mcp::tool_name(&server, "resource");
        Self {
            pool,
            server,
            name,
            definition: DefinitionCache::new(),
        }
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

/// [`mcp_tools`] built purely from the schema cache: a hit contributes the
/// server's tools, a miss contributes nothing, and no MCP server is spawned
/// on this path. `open_session` uses it so attach never waits on MCP work —
/// the daemon's post-attach refresh swaps in the full set once it lands.
pub fn mcp_tools_cached(pool: Arc<McpPool>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    for (server, info) in pool.list_all_tools_cached() {
        tools.push(Arc::new(McpTool::new(pool.clone(), server, info)));
    }

    let mut servers_with_resources = BTreeSet::new();
    for (server, _) in pool.list_all_resources_cached() {
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
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
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

        let preview = preview_body(&capped);

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
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
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

        let preview = preview_body(&capped);

        let mut output = ToolOutput::text(capped);
        output = output.with_title(&self.name).with_preview(preview);
        Ok(output)
    }
}
