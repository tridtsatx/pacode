//! OpenAI-compatible `/chat/completions` adapter (spec §11).
//!
//! Request body: `model`, `messages` (system = static + "\n\n" + dynamic as the first
//! `system` message; assistant messages carry `tool_calls`; `Role::Tool` messages
//! become `{"role":"tool","tool_call_id":..,"content":..}`; reasoning blocks are NOT
//! sent), `tools` as `{"type":"function","function":{name,description,parameters}}`,
//! `tool_choice: "auto"` when tools are present, `stream: true`,
//! `stream_options: {"include_usage": true}`, `reasoning_effort` when reasoning is
//! enabled for the model (config `reasoning`, model config, or `None` = auto: send for
//! models whose id suggests reasoning support — keep the heuristic small), effort
//! mapped `low/medium/high`, `max` → `high` unless `effort_map` overrides, and
//! `extra_body` merged last.
//!
//! SSE parsing is tolerant: `delta.content`, `delta.reasoning_content` or
//! `delta.reasoning` or `delta.thinking` (string), `delta.tool_calls[i]` with `index`
//! (missing index = position in array), `finish_reason` (`stop` → EndTurn,
//! `tool_calls` → ToolUse, `length` → MaxTokens, `content_filter` → ContentFilter),
//! `usage` (`prompt_tokens`, `completion_tokens`, `completion_tokens_details.reasoning_tokens`,
//! `prompt_tokens_details.cached_tokens`), `[DONE]`. Unknown fields ignored. An
//! `error` object in the stream or a non-2xx status becomes `ProviderError`.
//!
//! Retries: connection/429/5xx retried with backoff 1 s → 30 s (jitter), up to
//! `max_retries`; a stream idle longer than `stream_idle_secs × effort factor` ends with
//! `ProviderError::IdleTimeout`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use codeapp_types::{ModelInfo, Pricing, ProviderConfig, ProviderDefaults};
use serde_json::Value;

use crate::{CompletionRequest, EventStream, Provider, ProviderError};

pub struct OpenAiCompat {
    _private: (),
}

impl OpenAiCompat {
    /// `api_key` is already resolved by `codeapp-config`.
    pub fn new(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: Option<String>,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let _ = (id.into(), cfg, defaults, api_key, pricing);
        todo!("OpenAiCompat::new")
    }

    /// The JSON body for `req` (public for tests and `codeapp run --debug-request`).
    pub fn build_body(&self, req: &CompletionRequest) -> Value {
        let _ = req;
        todo!("OpenAiCompat::build_body")
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        todo!("OpenAiCompat::id")
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let _ = req;
        todo!("OpenAiCompat::complete")
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        todo!("OpenAiCompat::list_models")
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        let _ = model;
        todo!("OpenAiCompat::model_info")
    }
}

/// Incremental SSE parser: feed raw bytes, get complete `data:` payloads.
pub struct SseParser {
    _private: (),
}

impl SseParser {
    pub fn new() -> Self {
        todo!("SseParser::new")
    }

    /// Append bytes; returns the `data:` payloads of every complete event.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let _ = bytes;
        todo!("SseParser::feed")
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Translate one chat-completions chunk (already parsed JSON) into stream events.
/// `state` carries tool-call bookkeeping across chunks.
pub fn chunk_to_events(
    chunk: &Value,
    state: &mut ChunkState,
) -> Result<Vec<codeapp_types::StreamEvent>, ProviderError> {
    let _ = (chunk, state);
    todo!("openai_compat::chunk_to_events")
}

#[derive(Default)]
pub struct ChunkState {
    _private: (),
}
