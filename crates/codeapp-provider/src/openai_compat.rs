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

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use codeapp_types::{
    ContentBlock, Effort, ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, Role,
    prettify_model_name,
};
use rand::Rng;
use serde_json::Value;

pub use crate::sse::{ChunkState, SseParser, chunk_to_events};
use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[cfg(test)]
#[path = "openai_compat_tests.rs"]
mod openai_compat_tests;

const REASONING_KEYWORDS: &[&str] = &[
    "gemini",
    "o1",
    "o3",
    "o4",
    "gpt-5",
    "deepseek-r",
    "qwq",
    "thinking",
    "reason",
    "glm-5",
    "kimi-k2",
    "claude",
    "minimax",
];

pub struct OpenAiCompat {
    id: String,
    cfg: ProviderConfig,
    defaults: ProviderDefaults,
    api_key: Option<String>,
    pricing: BTreeMap<String, Pricing>,
    client: reqwest::Client,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
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
        let id = id.into();
        if cfg.base_url.trim().is_empty() {
            return Err(ProviderError::Config(format!(
                "provider '{id}' has empty base_url"
            )));
        }

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .user_agent(format!("codeapp/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Config(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            id,
            cfg,
            defaults,
            api_key,
            pricing,
            client,
            catalog_cache: Mutex::new(None),
        })
    }

    /// The JSON body for `req` (public for tests and `codeapp run --debug-request`).
    pub fn build_body(&self, req: &CompletionRequest) -> Value {
        let system_content = if req.system_dynamic.is_empty() {
            req.system_static.clone()
        } else if req.system_static.is_empty() {
            req.system_dynamic.clone()
        } else {
            format!("{}\n\n{}", req.system_static, req.system_dynamic)
        };

        let mut messages = Vec::with_capacity(1 + req.messages.len());
        messages.push(serde_json::json!({
            "role": "system",
            "content": system_content,
        }));

        for msg in &req.messages {
            match msg.role {
                Role::User => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": msg.text(),
                    }));
                }
                Role::System => {
                    messages.push(serde_json::json!({
                        "role": "system",
                        "content": msg.text(),
                    }));
                }
                Role::Assistant => {
                    let text = msg.text();
                    let mut tool_calls = Vec::new();
                    for block in &msg.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            tool_calls.push(serde_json::json!({
                                "id": id.as_str(),
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": input.to_string(),
                                },
                            }));
                        }
                    }

                    let mut assistant_msg = serde_json::Map::new();
                    assistant_msg.insert("role".to_string(), serde_json::json!("assistant"));
                    if tool_calls.is_empty() {
                        assistant_msg.insert("content".to_string(), serde_json::json!(text));
                    } else {
                        let content_val = if text.is_empty() {
                            Value::Null
                        } else {
                            Value::String(text)
                        };
                        assistant_msg.insert("content".to_string(), content_val);
                        assistant_msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
                    }
                    messages.push(Value::Object(assistant_msg));
                }
                Role::Tool => {
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            call_id, content, ..
                        } = block
                        {
                            messages.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": call_id.as_str(),
                                "content": content,
                            }));
                        }
                    }
                }
            }
        }

        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::json!(req.model));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), serde_json::json!(true));
        body.insert(
            "stream_options".to_string(),
            serde_json::json!({ "include_usage": true }),
        );

        if !req.tools.is_empty() {
            let tools_val: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body.insert("tools".to_string(), Value::Array(tools_val));
            body.insert("tool_choice".to_string(), serde_json::json!("auto"));
        }

        if let Some(max_tokens) = req.max_output_tokens {
            body.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
        }

        if self.reasoning_enabled_for(&req.model) {
            let effort = req.effort.unwrap_or(self.defaults.effort);
            let effort_str = match self.cfg.effort_map.get(effort.as_str()) {
                Some(override_val) => override_val.clone(),
                None => match effort {
                    Effort::Low => "low".to_string(),
                    Effort::Medium => "medium".to_string(),
                    Effort::High | Effort::XHigh | Effort::Max => "high".to_string(),
                },
            };
            body.insert(
                "reasoning_effort".to_string(),
                serde_json::json!(effort_str),
            );
        }

        let mut body_val = Value::Object(body);
        if let Some(extra) = &self.cfg.extra_body
            && extra.is_object()
        {
            deep_merge(&mut body_val, extra);
        }
        body_val
    }

    fn reasoning_enabled_for(&self, model: &str) -> bool {
        let model_cfg = self.cfg.models.iter().find(|m| m.id == model);
        if let Some(cfg) = model_cfg
            && let Some(r) = cfg.reasoning
        {
            return r;
        }
        if let Some(r) = self.cfg.reasoning {
            return r;
        }
        model_heuristic_reasoning(model)
    }
}

fn model_heuristic_reasoning(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    REASONING_KEYWORDS.iter().any(|&k| lower.contains(k))
}

fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, val) in source_map {
                match target_map.get_mut(key) {
                    Some(target_val) => deep_merge(target_val, val),
                    None => {
                        target_map.insert(key.clone(), val.clone());
                    }
                }
            }
        }
        (target, source) => {
            *target = source.clone();
        }
    }
}

async fn sleep_backoff(attempt: u32) {
    let exp = 1u64.checked_shl(attempt.min(5)).unwrap_or(32);
    let base_secs = (exp as f64).min(30.0);
    let delay_secs = {
        let mut rng = rand::rng();
        let jitter_ratio = rng.random_range(-0.2..=0.2);
        (base_secs * (1.0 + jitter_ratio)).max(0.01)
    };
    tokio::time::sleep(Duration::from_secs_f64(delay_secs)).await;
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let body = self.build_body(&req);
        let max_retries = self.defaults.max_retries;

        let mut attempt = 0;
        let response = loop {
            let mut req_builder = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&body);

            if let Some(key) = &self.api_key
                && !key.is_empty()
            {
                req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
            }

            for (k, v) in &self.cfg.headers {
                req_builder = req_builder.header(k, v);
            }

            match req_builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        break resp;
                    }

                    let status_u16 = status.as_u16();
                    if status_u16 == 401 || status_u16 == 403 {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::Auth(format!("{status_u16}: {text}")));
                    }

                    let is_retryable = status_u16 == 429 || status_u16 >= 500;
                    if is_retryable && attempt < max_retries {
                        let _ = resp.text().await;
                        sleep_backoff(attempt).await;
                        attempt += 1;
                        continue;
                    }

                    let text = resp.text().await.unwrap_or_default();
                    if status_u16 == 429 {
                        return Err(ProviderError::RateLimited(text));
                    }
                    return Err(ProviderError::Http {
                        status: status_u16,
                        message: text,
                    });
                }
                Err(err) => {
                    if attempt < max_retries {
                        sleep_backoff(attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(ProviderError::Transport(err.to_string()));
                }
            }
        };

        let effort = req.effort.unwrap_or(self.defaults.effort);
        Ok(crate::sse::create_event_stream(
            response,
            req.model,
            self.defaults.stream_idle_secs,
            effort,
        ))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if let Ok(guard) = self.catalog_cache.lock()
            && let Some(cached) = guard.as_ref()
        {
            return Ok(cached.clone());
        }

        let mut configured_models = Vec::new();
        let mut seen = HashSet::new();
        for m in &self.cfg.models {
            if seen.insert(m.id.clone()) {
                configured_models.push(self.model_info(&m.id));
            }
        }

        if !self.cfg.catalog {
            if let Ok(mut guard) = self.catalog_cache.lock() {
                *guard = Some(configured_models.clone());
            }
            return Ok(configured_models);
        }

        let url = format!("{}/models", self.cfg.base_url.trim_end_matches('/'));
        let mut req_builder = self.client.get(&url);
        if let Some(key) = &self.api_key
            && !key.is_empty()
        {
            req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
        }
        for (k, v) in &self.cfg.headers {
            req_builder = req_builder.header(k, v);
        }

        let response = match req_builder.send().await {
            Ok(resp) => resp,
            Err(err) => {
                if !configured_models.is_empty() {
                    log::warn!("failed to fetch models from {url}: {err}");
                    if let Ok(mut guard) = self.catalog_cache.lock() {
                        *guard = Some(configured_models.clone());
                    }
                    return Ok(configured_models);
                }
                return Err(ProviderError::Transport(err.to_string()));
            }
        };

        if !response.status().is_success() {
            if !configured_models.is_empty() {
                log::warn!(
                    "models endpoint {url} returned status {}",
                    response.status()
                );
                if let Ok(mut guard) = self.catalog_cache.lock() {
                    *guard = Some(configured_models.clone());
                }
                return Ok(configured_models);
            }
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status,
                message: text,
            });
        }

        let json_val = match response.json::<Value>().await {
            Ok(j) => j,
            Err(err) => {
                if !configured_models.is_empty() {
                    log::warn!("failed to parse JSON from {url}: {err}");
                    if let Ok(mut guard) = self.catalog_cache.lock() {
                        *guard = Some(configured_models.clone());
                    }
                    return Ok(configured_models);
                }
                return Err(ProviderError::Malformed(err.to_string()));
            }
        };

        let mut all_models = configured_models;
        if let Some(data) = json_val.get("data").and_then(|v| v.as_array()) {
            for item in data {
                if let Some(id) = item.get("id").and_then(|v| v.as_str())
                    && seen.insert(id.to_string())
                {
                    all_models.push(self.model_info(id));
                }
            }
        }

        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = Some(all_models.clone());
        }
        Ok(all_models)
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        let model_cfg = self.cfg.models.iter().find(|m| m.id == model);

        let display_name = model_cfg
            .and_then(|m| m.display_name.clone())
            .unwrap_or_else(|| prettify_model_name(model));

        let context_window = model_cfg
            .and_then(|m| m.context_window)
            .or(self.cfg.context_window);

        let supports_reasoning = model_cfg
            .and_then(|m| m.reasoning)
            .or(self.cfg.reasoning)
            .unwrap_or_else(|| model_heuristic_reasoning(model));

        let pricing = self.pricing.get(model).copied();

        ModelInfo {
            route: ModelRoute::new(self.id.clone(), model),
            display_name,
            context_window,
            supports_reasoning,
            pricing,
        }
    }
}
