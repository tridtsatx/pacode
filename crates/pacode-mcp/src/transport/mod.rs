//! Transport abstraction for MCP clients (Stdio and Streamable HTTP).

pub mod http;
pub mod sse;
pub mod stdio;

use std::sync::Arc;
use std::time::Duration;

pub use http::HttpTransport;
use serde_json::Value;
pub use stdio::StdioTransport;

use super::protocol::JsonRpcResponse;
use super::{McpError, SamplingHandler};

pub enum Transport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

impl Transport {
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, McpError> {
        match self {
            Transport::Stdio(t) => t.request(method, params, timeout).await,
            Transport::Http(t) => t.request(method, params, timeout).await,
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        match self {
            Transport::Stdio(t) => t.notify(method, params).await,
            Transport::Http(t) => t.notify(method, params).await,
        }
    }

    pub fn is_alive(&self) -> bool {
        match self {
            Transport::Stdio(t) => t.is_alive(),
            Transport::Http(t) => t.is_alive(),
        }
    }

    pub fn set_sampling_handler(&self, handler: Arc<dyn SamplingHandler>) {
        match self {
            Transport::Stdio(t) => t.set_sampling_handler(handler),
            Transport::Http(t) => t.set_sampling_handler(handler),
        }
    }

    pub fn set_sampling_config(&self, enabled: bool, max_tokens: u32) {
        match self {
            Transport::Stdio(t) => t.set_sampling_config(enabled, max_tokens),
            Transport::Http(t) => t.set_sampling_config(enabled, max_tokens),
        }
    }

    pub async fn shutdown(&self) {
        match self {
            Transport::Stdio(t) => t.shutdown().await,
            Transport::Http(t) => t.shutdown().await,
        }
    }
}
