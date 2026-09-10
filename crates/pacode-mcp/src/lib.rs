//! MCP (Model Context Protocol) client over stdio and Streamable HTTP JSON-RPC 2.0.
//!
//! - [`McpClient`]: one server process or HTTP endpoint. `initialize` handshake
//!   (offers `2025-06-18`, accepts server version), `notifications/initialized`,
//!   `tools/list`, `tools/call`, `resources/list`, `resources/read`, `prompts/list`,
//!   `prompts/get`.
//! - [`McpPool`]: server registry, lazy start on first use, schema cache on disk
//!   (`cache_dir/<server>.json`, keyed by fingerprint and version) for tools, resources,
//!   and prompts.
//! - Server status: `ServerStatus` enum (`Stopped`, `Starting`, `Ready`, `Failed`).

pub mod cache;
pub mod client;
pub mod pool;
pub mod protocol;
pub mod transport;

use std::path::PathBuf;

pub use cache::fingerprint;
pub use client::McpClient;
pub use pool::McpPool;
pub use transport::{HttpTransport, StdioTransport, Transport};

fn default_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Ready {
        tools: usize,
        resources: usize,
        prompts: usize,
    },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(
        default = "default_schema",
        rename = "inputSchema",
        alias = "input_schema"
    )]
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpCallResult {
    /// Text content blocks joined by newlines; non-text blocks summarised as `[image]` etc.
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType", alias = "mime_type")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SamplingMessage {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SamplingRequest {
    pub messages: Vec<SamplingMessage>,
    #[serde(default, rename = "systemPrompt", alias = "system_prompt")]
    pub system_prompt: Option<String>,
    #[serde(default, rename = "maxTokens", alias = "max_tokens")]
    pub max_tokens: Option<u32>,
    #[serde(default, rename = "modelPreferences", alias = "model_preferences")]
    pub model_preferences: Option<serde_json::Value>,
    #[serde(default, rename = "includeContext", alias = "include_context")]
    pub include_context: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default, rename = "stopSequences", alias = "stop_sequences")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SamplingResponse {
    pub role: String,
    pub content: serde_json::Value,
    pub model: String,
    #[serde(default, rename = "stopReason", alias = "stop_reason")]
    pub stop_reason: Option<String>,
}

impl SamplingResponse {
    pub fn text(model: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: serde_json::json!({
                "type": "text",
                "text": text.into(),
            }),
            model: model.into(),
            stop_reason: Some("endTurn".to_string()),
        }
    }
}

#[async_trait::async_trait]
pub trait SamplingHandler: Send + Sync {
    async fn create_message(&self, req: SamplingRequest) -> Result<SamplingResponse, McpError>;
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server '{0}' is not configured")]
    UnknownServer(String),
    #[error("server '{0}' is disabled")]
    ServerDisabled(String),
    #[error("failed to start '{server}': {source}")]
    Spawn {
        server: String,
        source: std::io::Error,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("request timed out after {0}s")]
    Timeout(u64),
    #[error("server error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("server exited")]
    Closed,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// `<server>__<tool>`; split back with [`split_tool_name`].
///
/// Both halves are sanitised: providers constrain function names to
/// `[A-Za-z0-9_.:-]` (Gemini rejects the request outright, OpenAI is stricter
/// still), and a plugin-provided server is named `<plugin>/<server>`, so the
/// raw join would emit a name every strict provider refuses.
pub fn tool_name(server: &str, tool: &str) -> String {
    let server = sanitize_name_part(server);
    let tool = sanitize_name_part(tool);
    format!("{server}__{tool}")
}

/// Map a name part onto the character set every provider accepts. Anything
/// outside `[A-Za-z0-9_.:-]` becomes `_`; a leading digit, dot, colon or dash
/// gets an `_` prefix because a function name must start with a letter or an
/// underscore.
pub(crate) fn sanitize_name_part(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for ch in part.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out
        .chars()
        .next()
        .is_some_and(|c| !c.is_ascii_alphabetic() && c != '_')
    {
        out.insert(0, '_');
    }
    out
}

pub fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    name.split_once("__")
}

/// Where the schema cache lives; `None` disables caching.
pub type CacheDir = Option<PathBuf>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name_and_split() {
        let full = tool_name("server1", "my_tool");
        assert_eq!(full, "server1__my_tool");
        assert_eq!(split_tool_name(&full), Some(("server1", "my_tool")));

        assert_eq!(split_tool_name("no_separator"), None);
        assert_eq!(split_tool_name("s__t1__t2"), Some(("s", "t1__t2")));
    }

    #[test]
    fn test_sampling_response_text_helper() {
        let resp = SamplingResponse::text("claude", "hi");
        assert_eq!(resp.role, "assistant");
        assert_eq!(resp.model, "claude");
        assert_eq!(resp.content["text"], "hi");
        assert_eq!(resp.stop_reason.as_deref(), Some("endTurn"));
    }
}
