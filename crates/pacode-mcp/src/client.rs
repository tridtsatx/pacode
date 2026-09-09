//! One MCP server client over Stdio or HTTP transport.

use std::sync::Arc;
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;

use crate::protocol::{PROTOCOL_VERSION_2024_11_05, PROTOCOL_VERSION_2025_06_18};
use crate::transport::{HttpTransport, StdioTransport, Transport};
use crate::{
    McpCallResult, McpError, McpPrompt, McpPromptArgument, McpResource, McpToolInfo,
    SamplingHandler,
};

pub struct McpClient {
    name: String,
    timeout_secs: u64,
    transport: Transport,
    protocol_version: String,
    capabilities: Value,
    server_info: Value,
}

impl McpClient {
    pub async fn start(
        name: &str,
        cfg: &McpServerConfig,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, McpError> {
        Self::start_with_sampling(name, cfg, cwd, None, true, 2048).await
    }

    pub async fn start_with_sampling(
        name: &str,
        cfg: &McpServerConfig,
        cwd: Option<&std::path::Path>,
        sampling_handler: Option<Arc<dyn SamplingHandler>>,
        sampling_enabled: bool,
        sampling_max_tokens: u32,
    ) -> Result<Self, McpError> {
        let is_http = cfg.url.as_ref().is_some_and(|u| !u.trim().is_empty());

        let transport = if is_http {
            Transport::Http(
                HttpTransport::start(
                    name,
                    cfg,
                    sampling_handler,
                    sampling_enabled,
                    sampling_max_tokens,
                )
                .await?,
            )
        } else {
            Transport::Stdio(
                StdioTransport::start(
                    name,
                    cfg,
                    cwd,
                    sampling_handler,
                    sampling_enabled,
                    sampling_max_tokens,
                )
                .await?,
            )
        };

        let timeout_secs = if cfg.timeout_secs == 0 {
            60
        } else {
            cfg.timeout_secs
        };

        // Handshake: offer 2025-06-18, accept whatever server returns
        let init_params = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION_2025_06_18,
            "capabilities": {
                "sampling": {}
            },
            "clientInfo": {
                "name": "pacode",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let handshake_timeout = Duration::from_secs(timeout_secs);
        let resp = match transport
            .request("initialize", Some(init_params), handshake_timeout)
            .await
        {
            Ok(r) => r,
            Err(err) => {
                transport.shutdown().await;
                return Err(err);
            }
        };

        let result = resp.result.unwrap_or_else(|| serde_json::json!({}));
        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(PROTOCOL_VERSION_2024_11_05)
            .to_string();

        let capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        let server_info = result
            .get("serverInfo")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        if let Err(err) = transport.notify("notifications/initialized", None).await {
            transport.shutdown().await;
            return Err(err);
        }

        Ok(Self {
            name: name.to_string(),
            timeout_secs,
            transport,
            protocol_version,
            capabilities,
            server_info,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    pub fn server_info(&self) -> &Value {
        &self.server_info
    }

    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    pub fn set_sampling_handler(&self, handler: Arc<dyn SamplingHandler>) {
        self.transport.set_sampling_handler(handler);
    }

    pub fn set_sampling_config(&self, enabled: bool, max_tokens: u32) {
        self.transport.set_sampling_config(enabled, max_tokens);
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let timeout = Duration::from_secs(self.timeout_secs);

        loop {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let resp = self
                .transport
                .request("tools/list", params, timeout)
                .await?;
            let res = resp.result.ok_or_else(|| {
                McpError::Protocol("missing result in tools/list response".to_string())
            })?;

            if let Some(items) = res.get("tools").and_then(|t| t.as_array()) {
                for item in items {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input_schema = item
                        .get("inputSchema")
                        .or_else(|| item.get("input_schema"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
                    tools.push(McpToolInfo {
                        name,
                        description,
                        input_schema,
                    });
                }
            }

            match res
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                Some(nc) => cursor = Some(nc.to_string()),
                None => break,
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<McpCallResult, McpError> {
        let arguments = if args.is_null() {
            serde_json::json!({})
        } else {
            args
        };
        let params = serde_json::json!({ "name": tool, "arguments": arguments });
        let resp = self
            .transport
            .request("tools/call", Some(params), timeout)
            .await?;
        let res = resp.result.ok_or_else(|| {
            McpError::Protocol("missing result in tools/call response".to_string())
        })?;

        let mut parts = Vec::new();
        if let Some(content_array) = res.get("content").and_then(|c| c.as_array()) {
            for block in content_array {
                let typ = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if typ == "text" {
                    parts.push(
                        block
                            .get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                } else if !typ.is_empty() {
                    parts.push(format!("[{typ}]"));
                } else {
                    parts.push("[unknown]".to_string());
                }
            }
        } else if let Some(text) = res.get("content").and_then(|c| c.as_str()) {
            parts.push(text.to_string());
        }

        let is_error = res
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(McpCallResult {
            content: parts.join("\n"),
            is_error,
        })
    }

    pub async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        let timeout = Duration::from_secs(self.timeout_secs);

        loop {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let resp = match self
                .transport
                .request("resources/list", params, timeout)
                .await
            {
                Ok(r) => r,
                Err(McpError::Server { code: -32601, .. }) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };

            let res = resp.result.ok_or_else(|| {
                McpError::Protocol("missing result in resources/list response".to_string())
            })?;

            if let Some(items) = res.get("resources").and_then(|r| r.as_array()) {
                for item in items {
                    let uri = item
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let mime_type = item
                        .get("mimeType")
                        .or_else(|| item.get("mime_type"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    resources.push(McpResource {
                        uri,
                        name,
                        description,
                        mime_type,
                    });
                }
            }

            match res
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                Some(nc) => cursor = Some(nc.to_string()),
                None => break,
            }
        }
        Ok(resources)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<String, McpError> {
        let timeout = Duration::from_secs(self.timeout_secs);
        let params = serde_json::json!({ "uri": uri });
        let resp = self
            .transport
            .request("resources/read", Some(params), timeout)
            .await?;
        let res = resp.result.ok_or_else(|| {
            McpError::Protocol("missing result in resources/read response".to_string())
        })?;

        let mut parts = Vec::new();
        if let Some(contents) = res.get("contents").and_then(|c| c.as_array()) {
            for item in contents {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(text.to_string());
                } else if item.get("blob").is_some() {
                    let mime = item
                        .get("mimeType")
                        .or_else(|| item.get("mime_type"))
                        .and_then(|v| v.as_str());
                    match mime {
                        Some(m) => parts.push(format!("[blob: {m}]")),
                        None => parts.push("[blob]".to_string()),
                    }
                } else {
                    parts.push("[unknown]".to_string());
                }
            }
        } else if let Some(text) = res.get("text").and_then(|t| t.as_str()) {
            parts.push(text.to_string());
        }

        Ok(parts.join("\n"))
    }

    pub async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
        let mut prompts = Vec::new();
        let mut cursor: Option<String> = None;
        let timeout = Duration::from_secs(self.timeout_secs);

        loop {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let resp = match self
                .transport
                .request("prompts/list", params, timeout)
                .await
            {
                Ok(r) => r,
                Err(McpError::Server { code: -32601, .. }) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };

            let res = resp.result.ok_or_else(|| {
                McpError::Protocol("missing result in prompts/list response".to_string())
            })?;

            if let Some(items) = res.get("prompts").and_then(|p| p.as_array()) {
                for item in items {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let mut arguments = Vec::new();
                    if let Some(args_arr) = item.get("arguments").and_then(|a| a.as_array()) {
                        for arg in args_arr {
                            let arg_name = arg
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arg_desc = arg
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let required = arg
                                .get("required")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            arguments.push(McpPromptArgument {
                                name: arg_name,
                                description: arg_desc,
                                required,
                            });
                        }
                    }
                    prompts.push(McpPrompt {
                        name,
                        description,
                        arguments,
                    });
                }
            }

            match res
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                Some(nc) => cursor = Some(nc.to_string()),
                None => break,
            }
        }
        Ok(prompts)
    }

    pub async fn get_prompt(&self, name: &str, args: Value) -> Result<String, McpError> {
        let timeout = Duration::from_secs(self.timeout_secs);
        let params = serde_json::json!({ "name": name, "arguments": args });
        let resp = self
            .transport
            .request("prompts/get", Some(params), timeout)
            .await?;
        let res = resp.result.ok_or_else(|| {
            McpError::Protocol("missing result in prompts/get response".to_string())
        })?;

        let mut rendered_messages = Vec::new();
        if let Some(messages) = res.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                let text = match msg.get("content") {
                    Some(Value::Object(obj)) => {
                        let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if typ == "text" {
                            obj.get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        } else if !typ.is_empty() {
                            format!("[{typ}]")
                        } else {
                            String::new()
                        }
                    }
                    Some(Value::Array(arr)) => {
                        let mut block_texts = Vec::new();
                        for block in arr {
                            let typ = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if typ == "text" {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                    block_texts.push(t.to_string());
                                }
                            } else if !typ.is_empty() {
                                block_texts.push(format!("[{typ}]"));
                            }
                        }
                        block_texts.join("\n")
                    }
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                if !text.is_empty() {
                    rendered_messages.push(text);
                }
            }
        }

        Ok(rendered_messages.join("\n\n"))
    }

    pub async fn shutdown(&self) {
        self.transport.shutdown().await;
    }
}
