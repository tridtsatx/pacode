//! Devin wire types and request builders.
//!
//! Reconstructed from the Devin CLI 3000.10.21 binary symbol table.
//! Response types are verified against the symbol table, while request structures
//! are reconstructed from embedded string references until live captured traffic
//! confirms them.

use pacode_types::{ContentBlock, ProviderConfig, ProviderDefaults, Role};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 1. Reconstructed Request Types
// ---------------------------------------------------------------------------

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GetCliModelConfigsRequest {}

impl GetCliModelConfigsRequest {
    pub fn to_wire() -> Self {
        Self {}
    }
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssignModelRequest {
    pub model_uid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_uids: Option<Vec<String>>,
}

impl AssignModelRequest {
    pub fn to_wire(model_uid: &str) -> Self {
        Self {
            model_uid: model_uid.to_string(),
            harness_uids: None,
        }
    }
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DevinMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DevinTool {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DevinThinkingConfig {
    pub thinking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub messages: Vec<DevinMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<DevinTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<DevinThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

// NOTE: field names reconstructed from the devin CLI 3000.10.21 symbol table; the request side is unverified until captured traffic confirms it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GetChatMessageRequest {
    pub assignment_jwt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_request: Option<InferenceRequest>,
}

impl GetChatMessageRequest {
    pub fn to_wire(
        assignment_jwt: &str,
        req: &crate::CompletionRequest,
        defaults: &ProviderDefaults,
        cfg: &ProviderConfig,
    ) -> Self {
        let system_content = if req.system_dynamic.is_empty() {
            req.system_static.to_string()
        } else if req.system_static.is_empty() {
            req.system_dynamic.to_string()
        } else {
            format!("{}\n\n{}", req.system_static, req.system_dynamic)
        };

        let mut wire_messages = Vec::with_capacity(req.messages.len());
        for msg in &req.messages {
            match msg.role {
                Role::User => {
                    wire_messages.push(DevinMessage {
                        role: "user".to_string(),
                        content: msg.text(),
                        tool_name: None,
                        tool_call_id: None,
                    });
                }
                Role::System => {
                    wire_messages.push(DevinMessage {
                        role: "system".to_string(),
                        content: msg.text(),
                        tool_name: None,
                        tool_call_id: None,
                    });
                }
                Role::Assistant => {
                    wire_messages.push(DevinMessage {
                        role: "assistant".to_string(),
                        content: msg.text(),
                        tool_name: None,
                        tool_call_id: None,
                    });
                }
                Role::Tool => {
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            call_id, content, ..
                        } = block
                        {
                            wire_messages.push(DevinMessage {
                                role: "tool".to_string(),
                                content: content.clone(),
                                tool_name: None,
                                tool_call_id: Some(call_id.to_string()),
                            });
                        }
                    }
                }
            }
        }

        let wire_tools: Vec<DevinTool> = req
            .tools
            .iter()
            .map(|t| DevinTool {
                tool_name: t.name.clone(),
                description: Some(t.description.clone()),
                parameters: Some(t.input_schema.clone()),
            })
            .collect();

        let resolved_effort = req.effort.unwrap_or(defaults.effort);
        let effort_str = cfg
            .effort_map
            .get(resolved_effort.as_str())
            .cloned()
            .unwrap_or_else(|| resolved_effort.as_str().to_string());

        let thinking = Some(DevinThinkingConfig {
            thinking: true,
            effort: Some(effort_str),
        });

        let inference_request = InferenceRequest {
            messages: wire_messages,
            system: if system_content.is_empty() {
                None
            } else {
                Some(system_content)
            },
            tools: wire_tools,
            anthropic_variant: None,
            thinking,
            max_tokens: req.max_output_tokens,
            stop_reason: None,
        };

        Self {
            assignment_jwt: assignment_jwt.to_string(),
            inference_request: Some(inference_request),
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Confirmed Response Types
// ---------------------------------------------------------------------------

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ClientModelConfig {
    #[serde(default)]
    pub model_uid: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub supports_reasoning: Option<bool>,
}

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct GetCliModelConfigsResponse {
    #[serde(default)]
    pub client_model_configs: Vec<ClientModelConfig>,
    #[serde(default)]
    pub subagent_default_model_uid: Option<String>,
    #[serde(default)]
    pub default_override_model_config: Option<ClientModelConfig>,
}

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ModelAssignment {
    pub model_uid: String,
    pub assignment_jwt: String,
    #[serde(default)]
    pub harness_uids: Vec<String>,
}

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AssignModelResponse {
    pub assignment: ModelAssignment,
}

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct DevinUsage {
    #[serde(default, alias = "prompt_tokens")]
    pub input_tokens: u64,
    #[serde(default, alias = "completion_tokens")]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default, alias = "cache_read_tokens")]
    pub cache_read_input_tokens: u64,
    #[serde(default, alias = "cache_write_tokens")]
    pub cache_creation_input_tokens: u64,
}

// NOTE: response field names confirmed from devin CLI 3000.10.21 symbol table.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct GetChatMessageResponse {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub delta_text: Option<String>,
    #[serde(default)]
    pub delta_thinking: Option<String>,
    #[serde(default)]
    pub delta_signature: Option<String>,
    #[serde(default)]
    pub delta_tokens: Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<DevinUsage>,
    #[serde(default)]
    pub redact: Option<serde_json::Value>,
}
