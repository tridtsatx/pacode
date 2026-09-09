//! Plugin tool proxy: wraps a plugin-declared tool definition and calls `PluginHost`.

use std::sync::Arc;

use async_trait::async_trait;
use pacode_plugin::{PluginHost, PluginToolDef};
use serde_json::Value;

use super::helpers::cap_output;
use crate::{ACCEPT_LARGE_OUTPUT_KEY, INTENT_KEY, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub struct PluginTool {
    host: Arc<PluginHost>,
    def: PluginToolDef,
}

impl PluginTool {
    pub fn new(host: Arc<PluginHost>, def: PluginToolDef) -> Self {
        Self { host, def }
    }
}

/// Build tool proxies for every tool exported by plugins on the host.
pub fn plugin_tools(host: Arc<PluginHost>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for def in host.tools() {
        tools.push(Arc::new(PluginTool::new(Arc::clone(&host), def)));
    }
    tools
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn schema(&self) -> Value {
        self.def.schema.clone()
    }

    /// Plugin tools are treated as `Network` (safest existing kind matching MCP tools).
    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

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

        let result = self
            .host
            .call_tool(&self.def.name, args)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;

        let content_str = match &result {
            Value::String(s) => s.clone(),
            _ => serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()),
        };

        let capped = cap_output(&content_str, accept_large_output, ctx.output_cap_chars);

        let preview = capped
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .to_string();

        let mut output = ToolOutput::text(capped);
        output = output.with_title(&self.def.name).with_preview(preview);
        Ok(output)
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod plugin_tests;
