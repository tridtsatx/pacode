//! MCP sampling handler implementation using core's provider and session route.

use std::sync::Weak;

use async_trait::async_trait;
use futures::StreamExt;
use pacode_mcp::{McpError, SamplingHandler, SamplingRequest, SamplingResponse};
use pacode_types::{ContentBlock, Message, Role, StopReason, StreamEvent};

use crate::Core;

pub struct CoreSamplingHandler {
    core: Weak<Core>,
}

impl CoreSamplingHandler {
    pub fn new(core: Weak<Core>) -> Self {
        Self { core }
    }
}

#[async_trait]
impl SamplingHandler for CoreSamplingHandler {
    async fn create_message(&self, req: SamplingRequest) -> Result<SamplingResponse, McpError> {
        let core = self.core.upgrade().ok_or(McpError::Closed)?;

        let config = core.config();
        if !config.mcp.sampling {
            return Err(McpError::Protocol(
                "MCP sampling is disabled in configuration".to_string(),
            ));
        }

        let (provider, model_name) = {
            let session_model = {
                let sessions = core.sessions.read().unwrap_or_else(|p| p.into_inner());
                sessions.values().last().map(|s| s.meta().model)
            };
            let providers = core.providers();
            let route = session_model
                .or_else(|| providers.default_route().cloned())
                .ok_or_else(|| {
                    McpError::Protocol("no provider route available for sampling".to_string())
                })?;
            let provider = providers
                .resolve(&route)
                .await
                .map_err(|e| McpError::Protocol(e.to_string()))?;
            (provider, route.model)
        };

        let cap = config.mcp.sampling_max_tokens;
        let max_tokens = match req.max_tokens {
            Some(requested) => requested.min(cap),
            None => cap,
        };

        let mut messages = Vec::new();
        for msg in req.messages {
            let role = if msg.role == "assistant" {
                Role::Assistant
            } else {
                Role::User
            };
            let text = match msg.content {
                serde_json::Value::String(s) => s,
                serde_json::Value::Object(obj) => obj
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                serde_json::Value::Array(arr) => {
                    let mut parts: Vec<String> = Vec::new();
                    for item in arr {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                            parts.push(t.to_string());
                        }
                    }
                    parts.join("\n")
                }
                _ => String::new(),
            };
            messages.push(Message::new(role, vec![ContentBlock::Text { text }]));
        }

        let comp_req = pacode_provider::CompletionRequest {
            model: model_name.clone(),
            system_static: req.system_prompt.unwrap_or_default(),
            system_dynamic: String::new(),
            messages,
            tools: Vec::new(),
            effort: None,
            max_output_tokens: Some(max_tokens),
        };

        let mut stream = provider
            .complete(comp_req)
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        let mut text_acc = String::new();
        let mut stop_reason = Some("endTurn".to_string());

        while let Some(event_res) = stream.next().await {
            match event_res {
                Ok(StreamEvent::TextDelta { text }) => {
                    text_acc.push_str(&text);
                }
                Ok(StreamEvent::MessageEnd { stop }) => {
                    stop_reason = Some(match stop {
                        StopReason::EndTurn => "endTurn".to_string(),
                        StopReason::ToolUse => "toolUse".to_string(),
                        StopReason::MaxTokens => "maxTokens".to_string(),
                        StopReason::ContentFilter => "contentFilter".to_string(),
                        StopReason::Other(s) => s,
                    });
                }
                Ok(
                    StreamEvent::MessageStart { .. }
                    | StreamEvent::ReasoningDelta { .. }
                    | StreamEvent::ReasoningSignature { .. }
                    | StreamEvent::ToolCallStart { .. }
                    | StreamEvent::ToolCallArgsDelta { .. }
                    | StreamEvent::Usage(_),
                ) => {}
                Err(e) => return Err(McpError::Protocol(e.to_string())),
            }
        }

        Ok(SamplingResponse {
            role: "assistant".to_string(),
            content: serde_json::json!({
                "type": "text",
                "text": text_acc,
            }),
            model: model_name,
            stop_reason,
        })
    }
}

#[cfg(test)]
#[path = "sampling_tests.rs"]
mod sampling_tests;
