//! Model providers. v1 ships one adapter, [`OpenAiCompat`], for any OpenAI-compatible
//! `/chat/completions` endpoint (Gemini via its OpenAI endpoint, Ollama, OpenRouter,
//! vLLM, LM Studio, ...). [`ProviderRegistry`] resolves `provider/model` routes from
//! the config.
//!
//! Submodules (to implement):
//! - `openai_compat`: request building, SSE parsing, retries, `/models` catalog
//! - `registry`: `ProviderRegistry` built from `Config`
//! - `mock`: `MockProvider` with scripted responses (behind `cfg(any(test, feature = "mock"))`
//!   and always compiled for downstream tests via the `mock` feature)

pub mod mock;
pub mod openai_compat;
pub mod registry;
pub mod sse;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use pacode_types::{Effort, Message, ModelInfo, StreamEvent, ToolDefinition};

pub use mock::MockProvider;
pub use openai_compat::{OpenAiCompat, redact};
pub use registry::ProviderRegistry;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRequest {
    pub model: String,
    /// Static system prompt first (prefix cache), dynamic part second.
    pub system_static: String,
    pub system_dynamic: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub effort: Option<Effort>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("request failed ({status}): {message}")]
    Http { status: u16, message: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("stream idle for {0}s")]
    IdleTimeout(u64),
    #[error("malformed response: {0}")]
    Malformed(String),
    #[error("cancelled")]
    Cancelled,
}

impl ProviderError {
    /// Worth retrying with backoff (transport, 429, 5xx, idle timeout).
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::RateLimited(_)
            | ProviderError::Transport(_)
            | ProviderError::IdleTimeout(_) => true,
            ProviderError::Http { status, .. } => *status >= 500,
            ProviderError::Config(_)
            | ProviderError::Auth(_)
            | ProviderError::Malformed(_)
            | ProviderError::Cancelled => false,
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Registry key (`bubna`, `ollama`).
    fn id(&self) -> &str;
    /// Open a streaming completion. Retries of the *connection* happen inside; a
    /// stream that dies mid-way is surfaced as an error item and the core decides.
    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError>;
    /// Live catalog (`GET /models`) merged with configured models.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    /// Static knowledge about a model (context window, reasoning) without a network call.
    fn model_info(&self, model: &str) -> ModelInfo;
}
