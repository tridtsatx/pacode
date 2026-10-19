//! Model providers. [`OpenAiCompat`] serves every OpenAI-compatible `/chat/completions`
//! endpoint (OpenRouter, Kimi, Z.AI, OpenCode, Gemini via its OpenAI endpoint, OpenAI,
//! Ollama, LM Studio, custom gateways); `codex` speaks the OpenAI Responses API for the
//! ChatGPT login; `devin` is its own transport. [`ProviderRegistry`] resolves
//! `provider/model` routes from the config and the builtin table.
//!
//! Submodules:
//! - `builtin`: the builtin provider table (ids, URLs, env vars, default models)
//! - `compat`: per-endpoint wire quirks applied by `openai_compat`
//! - `dialect`: the wire-protocol table that turns a config entry into a transport
//! - `governor`: per-account admission control (adaptive AIMD limit, FIFO queue, cooldown)
//! - `openai_compat`: request building, SSE parsing, retries, `/models` catalog
//! - `registry`: `ProviderRegistry` built from `Config`
//! - `mock`: `MockProvider` with scripted responses (behind `cfg(any(test, feature = "mock"))`
//!   and always compiled for downstream tests via the `mock` feature)

// Kept for the `anthropic` dialect row (`dialect.rs`); the registry refuses to build
// it since the Anthropic login was removed (see `registry::build_transport`).
pub mod anthropic;
pub mod builtin;
pub mod catalog_cache;
pub mod catalog_efforts;
#[cfg(test)]
mod catalog_flight_tests;
pub mod codex;
pub mod compat;
pub mod devin;
pub mod dialect;
pub mod error_body;
mod governor;
pub mod image;
mod image_cache;
pub mod mock;
pub mod model_meta;
pub mod openai_compat;
pub mod opencode;
mod post;
pub mod registry;
mod registry_cache;
mod request_body;
mod retry;
pub mod sse;
mod sse_typed;
pub mod vision;

use std::borrow::Cow;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::Stream;
use pacode_types::{Effort, Message, ModelInfo, StreamEvent, ToolDefinition};

pub use builtin::{BuiltinModel, BuiltinProvider, builtin_providers, find_builtin};
pub use catalog_cache::{
    CATALOG_CACHE_VERSION, CatalogCache, DEFAULT_CATALOG_TTL_SECS, DEFAULT_MAX_MODELS_PER_PROVIDER,
};
pub use compat::{Compat, MaxTokensField, ReasoningWire, ToolIdRule};
pub use error_body::error_body_message;
pub use mock::MockProvider;
pub use openai_compat::{OpenAiCompat, redact};
pub use registry::{ProviderRegistry, ReloadReport};
pub use retry::RetryBudget;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

pub const DEFAULT_PROVIDER_CONCURRENCY: usize = 128;
pub const MAX_PROVIDER_CONCURRENCY: usize = 1024;
pub const PROVIDER_QUEUE_ITEM_LIMIT: usize = 4096;
pub const PROVIDER_QUEUE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;

static PROVIDER_CONCURRENCY: AtomicUsize = AtomicUsize::new(DEFAULT_PROVIDER_CONCURRENCY);
static PROVIDER_QUEUE_ITEMS: AtomicUsize = AtomicUsize::new(PROVIDER_QUEUE_ITEM_LIMIT);
static PROVIDER_QUEUE_BYTES: AtomicU64 = AtomicU64::new(PROVIDER_QUEUE_BYTE_LIMIT);

/// Admission control applied to every provider account: at most `max_concurrency`
/// requests in flight (the ceiling of the adaptive limit, see `governor`), the rest
/// parked in a FIFO queue bounded by items and bytes.
/// A zero queue cap never waits — it rejects instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderLimits {
    pub max_concurrency: usize,
    pub max_queue_items: usize,
    pub max_queue_bytes: u64,
}

impl Default for ProviderLimits {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_PROVIDER_CONCURRENCY,
            max_queue_items: PROVIDER_QUEUE_ITEM_LIMIT,
            max_queue_bytes: PROVIDER_QUEUE_BYTE_LIMIT,
        }
    }
}

impl ProviderLimits {
    pub fn new(
        max_concurrency: usize,
        max_queue_items: usize,
        max_queue_bytes: u64,
    ) -> Result<Self, ProviderError> {
        let limits = Self {
            max_concurrency,
            max_queue_items,
            max_queue_bytes,
        };
        if max_concurrency == 0 || max_concurrency > MAX_PROVIDER_CONCURRENCY {
            return Err(ProviderError::Config(format!(
                "provider concurrency must be between 1 and {MAX_PROVIDER_CONCURRENCY}"
            )));
        }
        Ok(limits)
    }

    pub fn with_concurrency(max_concurrency: usize) -> Result<Self, ProviderError> {
        Self::new(
            max_concurrency,
            PROVIDER_QUEUE_ITEM_LIMIT,
            PROVIDER_QUEUE_BYTE_LIMIT,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRequest {
    pub model: String,
    /// Static system prompt first (prefix cache), dynamic part second.
    pub system_static: String,
    pub system_dynamic: String,
    /// Shared message refs: `History::for_model` hands over refcount bumps
    /// instead of deep-cloning every content block on each model step, and a
    /// request clone (mock capture, retry state) is cheap for the same reason.
    /// Providers only ever read them.
    pub messages: Vec<Arc<Message>>,
    pub tools: Vec<ToolDefinition>,
    pub effort: Option<Effort>,
    pub max_output_tokens: Option<u32>,
}

/// Test helper: wrap owned fixture messages into the shared refs a request
/// carries — `shared(vec![Message::user("x")])` keeps fixtures readable.
#[cfg(test)]
pub(crate) fn shared(messages: Vec<Message>) -> Vec<Arc<Message>> {
    messages.into_iter().map(Arc::new).collect()
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

/// A reading of every live provider governor: in-flight and queued requests,
/// high-water marks, refusals, cooldowns, and how long callers waited for a
/// permit. Cheap enough for a request handler, and the thing to look at first
/// when a session with many agents feels slow — a non-zero wait says the
/// concurrency ceiling is the binding constraint rather than the model.
pub fn provider_stats() -> pacode_types::ProviderStats {
    governor::provider_stats()
}

pub fn provider_concurrency_ceiling() -> usize {
    PROVIDER_CONCURRENCY.load(Ordering::Acquire)
}

pub fn provider_limits() -> ProviderLimits {
    ProviderLimits {
        max_concurrency: provider_concurrency_ceiling(),
        max_queue_items: PROVIDER_QUEUE_ITEMS.load(Ordering::Acquire),
        max_queue_bytes: PROVIDER_QUEUE_BYTES.load(Ordering::Acquire),
    }
}

pub fn apply_provider_limits(limits: ProviderLimits) -> Result<ProviderLimits, ProviderError> {
    ProviderLimits::new(
        limits.max_concurrency,
        limits.max_queue_items,
        limits.max_queue_bytes,
    )?;
    PROVIDER_CONCURRENCY.store(limits.max_concurrency, Ordering::Release);
    PROVIDER_QUEUE_ITEMS.store(limits.max_queue_items, Ordering::Release);
    PROVIDER_QUEUE_BYTES.store(limits.max_queue_bytes, Ordering::Release);
    governor::apply_provider_limits(limits);
    Ok(limits)
}

pub fn set_provider_concurrency_ceiling(limit: usize) -> Result<usize, ProviderError> {
    let limits = apply_provider_limits(ProviderLimits::with_concurrency(limit)?)?;
    Ok(limits.max_concurrency)
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
    /// Live catalog fetch, bypassing any provider-internal cache.
    async fn refresh_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.list_models().await
    }
    /// Static knowledge about a model (context window, reasoning) without a network call.
    fn model_info(&self, model: &str) -> ModelInfo;
    /// Superseded by [`provider_stats`]: admission is per account, not per provider,
    /// so no provider reports a depth any more. Kept until its last caller moves.
    fn queue_depth(&self) -> usize {
        0
    }
    /// The name the model sees for the native tool `name`: what this provider
    /// puts in the request's tool schemas. Prompt text that names tools must
    /// go through it, or it names tools the model was never offered.
    fn wire_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(name)
    }
}
