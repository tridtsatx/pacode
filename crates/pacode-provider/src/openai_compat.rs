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
//! Images: a user message with images becomes a `content` array of
//! `{"type":"text"}` and `{"type":"image_url","image_url":{"url":"data:<mime>;base64,..."}}`
//! parts (bytes are loaded lazily from the image store). Chat completions cannot carry
//! images inside `tool` messages, so images returned by tools are sent as one `user`
//! follow-up message placed right after the whole run of `tool` messages (never between
//! them: every `tool_call_id` must be answered contiguously). Models without vision
//! fail before the request with `model <id> does not accept images`.
//!
//! SSE parsing is tolerant: `delta.content`, `delta.reasoning_content` or
//! `delta.reasoning` or `delta.thinking` (string), `delta.tool_calls[i]` with `index`
//! (missing index = position in array), `finish_reason` (`stop` → EndTurn,
//! `tool_calls` → ToolUse, `length` → MaxTokens, `content_filter` → ContentFilter),
//! `usage` (`prompt_tokens`, `completion_tokens`, `completion_tokens_details.reasoning_tokens`,
//! `prompt_tokens_details.cached_tokens`), `[DONE]`. Unknown fields ignored. An
//! `error` object in the stream or a non-2xx status becomes `ProviderError`.
//!
//! Retries (see `retry.rs`): connection/429/5xx retried with backoff 1 s → 30 s (jitter),
//! up to `max_retries`. A retryable error arriving *inside* the stream (gateways report
//! upstream 429/5xx as an `error` object in an HTTP 200 body) reopens the request as long
//! as the turn has not emitted any output yet. A stream idle longer than
//! `stream_idle_secs × effort factor` ends with `ProviderError::IdleTimeout`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{
    ContentBlock, ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, Role,
    prettify_model_name,
};
use serde_json::Value;

use crate::compat::{Compat, PLACEHOLDER, ReasoningWire};
pub use crate::sse::{ChunkState, SseParser, chunk_to_events};
use crate::vision;
use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[cfg(test)]
#[path = "openai_compat_tests.rs"]
mod openai_compat_tests;

pub struct OpenAiCompat {
    id: String,
    cfg: ProviderConfig,
    /// Wire quirks of the endpoint behind `cfg.base_url`.
    compat: Compat,
    defaults: ProviderDefaults,
    api_key: Option<String>,
    pricing: BTreeMap<String, Pricing>,
    client: reqwest::Client,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    /// First retry backoff step; only tuned down by tests.
    backoff_base: Duration,
}

impl OpenAiCompat {
    /// `api_key` is already resolved by `pacode-config`.
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

        let proxy_setting =
            pacode_net::ProxySetting::resolve(cfg.proxy.as_deref(), defaults.proxy.as_deref())
                .map_err(|e| {
                    ProviderError::Config(format!("provider '{id}' has invalid proxy: {e}"))
                })?;

        let client = pacode_net::client_builder(&proxy_setting)
            .map_err(|e| ProviderError::Config(format!("provider '{id}' proxy error: {e}")))?
            .connect_timeout(Duration::from_secs(30))
            .user_agent(format!("pacode/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Config(format!("failed to build HTTP client: {e}")))?;

        let compat = Compat::for_base_url(&cfg.base_url);

        Ok(Self {
            id,
            cfg,
            compat,
            defaults,
            api_key,
            pricing,
            client,
            catalog_cache: Mutex::new(None),
            backoff_base: crate::retry::DEFAULT_BACKOFF_BASE,
        })
    }

    /// Shorten the retry backoff schedule (tests drive real retries without real waits).
    #[cfg(test)]
    pub(crate) fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// The JSON body for `req` (public for tests and `pacode run --debug-request`).
    pub fn build_body(&self, req: &CompletionRequest) -> Value {
        let system_content = if req.system_dynamic.is_empty() {
            req.system_static.clone()
        } else if req.system_static.is_empty() {
            req.system_dynamic.clone()
        } else {
            format!("{}\n\n{}", req.system_static, req.system_dynamic)
        };

        let reasoning_on = self.reasoning_enabled_for(&req.model);

        let mut messages = Vec::with_capacity(1 + req.messages.len());
        messages.push(serde_json::json!({
            "role": "system",
            "content": system_content,
        }));

        let mut pending_tool_images: Vec<Value> = Vec::new();
        for msg in &req.messages {
            if msg.role != Role::Tool && !pending_tool_images.is_empty() {
                messages.push(tool_images_message(std::mem::take(
                    &mut pending_tool_images,
                )));
            }
            match msg.role {
                Role::User => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": user_content(msg),
                    }));
                }
                Role::System => {
                    messages.push(serde_json::json!({
                        "role": "system",
                        "content": msg.text(),
                    }));
                }
                Role::Assistant => {
                    let mut text = msg.text();
                    let mut tool_calls = Vec::new();
                    for block in &msg.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            tool_calls.push(serde_json::json!({
                                "id": self.compat.tool_id(id.as_str()),
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
                        if text.is_empty() && self.compat.text_for_tool_calls {
                            text = PLACEHOLDER.to_string();
                        }
                        let content_val = if text.is_empty() {
                            Value::Null
                        } else {
                            Value::String(text)
                        };
                        assistant_msg.insert("content".to_string(), content_val);
                        assistant_msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
                        if reasoning_on && self.compat.reasoning_content_for_tool_calls {
                            assistant_msg.insert(
                                "reasoning_content".to_string(),
                                serde_json::json!(PLACEHOLDER),
                            );
                        }
                    }
                    messages.push(Value::Object(assistant_msg));
                }
                Role::Tool => {
                    for block in &msg.content {
                        match block {
                            ContentBlock::ToolResult {
                                call_id, content, ..
                            } => {
                                messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": self.compat.tool_id(call_id.as_str()),
                                    "content": content,
                                }));
                            }
                            ContentBlock::Image { .. } => {
                                pending_tool_images.extend(image_parts(block));
                            }
                            ContentBlock::Text { .. }
                            | ContentBlock::Reasoning { .. }
                            | ContentBlock::ToolUse { .. } => {}
                        }
                    }
                }
            }
        }
        if !pending_tool_images.is_empty() {
            messages.push(tool_images_message(pending_tool_images));
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
                            "parameters": self.compat.clean_schema(&t.input_schema),
                        }
                    })
                })
                .collect();
            body.insert("tools".to_string(), Value::Array(tools_val));
            body.insert("tool_choice".to_string(), serde_json::json!("auto"));
        }

        let max_tokens = req.max_output_tokens.or(self.compat.default_max_tokens);
        if let Some(max_tokens) = max_tokens {
            body.insert(
                self.compat.max_tokens_field.key().to_string(),
                serde_json::json!(max_tokens),
            );
        }

        if reasoning_on {
            let wire = self.compat.reasoning_wire(&req.model);
            let effort = req.effort.unwrap_or(self.defaults.effort);
            let effort_str = match self.cfg.effort_map.get(effort.as_str()) {
                Some(override_val) => override_val.clone(),
                None => Compat::effort_str(wire, effort).to_string(),
            };
            match wire {
                ReasoningWire::Unsupported => {}
                ReasoningWire::ReasoningEffort | ReasoningWire::ReasoningEffortTiered => {
                    body.insert(
                        "reasoning_effort".to_string(),
                        serde_json::json!(effort_str),
                    );
                }
                ReasoningWire::ReasoningObject => {
                    body.insert(
                        "reasoning".to_string(),
                        serde_json::json!({ "effort": effort_str }),
                    );
                }
                ReasoningWire::ThinkingToggle => {
                    body.insert(
                        "thinking".to_string(),
                        serde_json::json!({ "type": "enabled" }),
                    );
                }
            }
        }

        let mut body_val = Value::Object(body);
        if let Some(extra) = &self.cfg.extra_body
            && extra.is_object()
        {
            deep_merge(&mut body_val, extra);
        }
        body_val
    }

    /// Vision flag of `model`: the live catalogue when it declared one, else the id
    /// heuristic.
    fn vision_for(&self, model: &str) -> bool {
        if let Ok(guard) = self.catalog_cache.lock()
            && let Some(found) = guard
                .as_ref()
                .and_then(|list| list.iter().find(|m| m.route.model == model))
        {
            return found.vision;
        }
        vision::heuristic_vision(model)
    }

    /// Reject images for text-only models; strip stale ones from old history.
    fn gate_images(&self, mut req: CompletionRequest) -> Result<CompletionRequest, ProviderError> {
        if !req.messages.iter().any(pacode_types::Message::has_images) {
            return Ok(req);
        }
        let vision = self.vision_for(&req.model);
        let gated = vision::gate_images(&req.messages, &req.model, vision)?.into_owned();
        req.messages = gated;
        Ok(req)
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
        self.compat.heuristic_reasoning(model)
    }

    pub fn clear_catalog_cache(&self) {
        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = None;
        }
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
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
                    let url_redacted = redact(&url);
                    log::warn!("failed to fetch models from {url_redacted}: {err}");
                    if let Ok(mut guard) = self.catalog_cache.lock() {
                        *guard = Some(configured_models.clone());
                    }
                    return Ok(configured_models);
                }
                return Err(ProviderError::Transport(err.to_string()));
            }
        };

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let redacted_text = redact(&text);
            log::trace!("models response body: {redacted_text}");
            if !configured_models.is_empty() {
                let url_redacted = redact(&url);
                log::warn!(
                    "models endpoint {url_redacted} returned status {status}: {redacted_text}"
                );
                if let Ok(mut guard) = self.catalog_cache.lock() {
                    *guard = Some(configured_models.clone());
                }
                return Ok(configured_models);
            }
            log::warn!("request failed ({status}): {redacted_text}");
            return Err(ProviderError::Http {
                status,
                message: text,
            });
        }

        let json_text = response.text().await.unwrap_or_default();
        let redacted_json = redact(&json_text);
        log::trace!("models response body: {redacted_json}");
        let json_val = match serde_json::from_str::<Value>(&json_text) {
            Ok(j) => j,
            Err(err) => {
                if !configured_models.is_empty() {
                    let url_redacted = redact(&url);
                    log::warn!("failed to parse JSON from {url_redacted}: {err}");
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
                if let Some(id) = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| self.compat.model_id(id))
                    && seen.insert(id.to_string())
                {
                    let mut info = self.model_info(id);
                    if let Some(declared) = vision::catalog_vision(item) {
                        info.vision = declared;
                    }
                    let pinned = self.cfg.reasoning.is_some();
                    crate::model_meta::apply_catalog_item(
                        &mut info,
                        item,
                        self.compat.reasoning_wire(id),
                        !pinned,
                    );
                    all_models.push(info);
                }
            }
        }

        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = Some(all_models.clone());
        }
        Ok(all_models)
    }
}

/// `content` of a user message: a plain string, or a parts array when it has images.
fn user_content(msg: &pacode_types::Message) -> Value {
    if !msg.has_images() {
        return Value::String(msg.text());
    }
    let mut parts = Vec::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    parts.push(serde_json::json!({ "type": "text", "text": text }));
                }
            }
            ContentBlock::Image { .. } => parts.extend(image_parts(block)),
            ContentBlock::Reasoning { .. }
            | ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. } => {}
        }
    }
    Value::Array(parts)
}

/// One `image_url` part for an image block, or a text note when the bytes are gone.
fn image_parts(block: &ContentBlock) -> Vec<Value> {
    let ContentBlock::Image {
        mime,
        hash,
        source_path,
        ..
    } = block
    else {
        return Vec::new();
    };
    match vision::data_url(mime, hash) {
        Ok(url) => vec![serde_json::json!({
            "type": "image_url",
            "image_url": { "url": url },
        })],
        Err(err) => {
            log::warn!("image {hash} left out of the request: {err}");
            vec![serde_json::json!({
                "type": "text",
                "text": vision::omitted_note(source_path.as_deref()),
            })]
        }
    }
}

/// The user follow-up carrying images that tools returned.
fn tool_images_message(parts: Vec<Value>) -> Value {
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": "Images returned by the tool call(s) above:",
    })];
    content.extend(parts);
    serde_json::json!({ "role": "user", "content": content })
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

// Test-only per-thread count of `redact` calls, used to prove trace work is skipped.
#[cfg(test)]
thread_local! {
    pub(crate) static REDACT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Redact sensitive information (`api_key`, `authorization`, `Bearer ...`) from log messages.
pub fn redact(input: &str) -> String {
    #[cfg(test)]
    REDACT_CALLS.with(|c| c.set(c.get() + 1));
    let mut result = redact_bearer(input);
    result = redact_keys(&result, &["api_key", "api-key", "apikey", "authorization"]);
    result
}

fn redact_bearer(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(pos) = find_case_insensitive(remaining, "bearer") {
        let is_word_boundary = if pos == 0 {
            true
        } else {
            let prev_char = remaining[..pos].chars().next_back().unwrap_or(' ');
            !prev_char.is_alphanumeric() && prev_char != '_'
        };

        if !is_word_boundary {
            out.push_str(&remaining[..pos + 6]);
            remaining = &remaining[pos + 6..];
            continue;
        }

        let after_bearer = &remaining[pos + 6..];
        let trimmed_colon = after_bearer.strip_prefix(':').unwrap_or(after_bearer);
        let ws_len = trimmed_colon.len() - trimmed_colon.trim_start_matches([' ', '\t']).len();
        if ws_len == 0 {
            out.push_str(&remaining[..pos + 6]);
            remaining = &remaining[pos + 6..];
            continue;
        }

        let prefix_len = pos + 6 + (after_bearer.len() - trimmed_colon.len()) + ws_len;
        out.push_str(&remaining[..prefix_len]);
        let token_start = &remaining[prefix_len..];

        if let Some(q) = token_start
            .chars()
            .next()
            .filter(|&c| c == '"' || c == '\'')
        {
            out.push(q);
            let val_content = &token_start[q.len_utf8()..];
            let val_len = find_closing_quote(val_content, q).unwrap_or(val_content.len());
            let val = &val_content[..val_len];
            if val.trim().is_empty() || val == "[REDACTED]" {
                out.push_str(val);
            } else {
                out.push_str("[REDACTED]");
            }
            if val_len < val_content.len() {
                out.push(q);
                remaining = &val_content[val_len + q.len_utf8()..];
            } else {
                remaining = "";
            }
        } else {
            let token_len = token_start
                .find(|c: char| {
                    c.is_whitespace()
                        || c == '"'
                        || c == '\''
                        || c == ','
                        || c == ';'
                        || c == '&'
                        || c == '}'
                        || c == ']'
                        || c == '\\'
                })
                .unwrap_or(token_start.len());

            let token = &token_start[..token_len];
            if !token.is_empty() && token != "[REDACTED]" {
                out.push_str("[REDACTED]");
            } else {
                out.push_str(token);
            }

            remaining = &token_start[token_len..];
        }
    }

    out.push_str(remaining);
    out
}

fn redact_keys(input: &str, keys: &[&str]) -> String {
    let mut current = input.to_string();
    for key in keys {
        current = redact_single_key(&current, key);
    }
    current
}

fn redact_single_key(input: &str, key: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(pos) = find_case_insensitive(remaining, key) {
        let is_boundary = if pos == 0 {
            true
        } else {
            let prev = remaining[..pos].chars().next_back().unwrap_or(' ');
            !prev.is_alphanumeric() && prev != '_' && prev != '-'
        };

        if !is_boundary {
            out.push_str(&remaining[..pos + key.len()]);
            remaining = &remaining[pos + key.len()..];
            continue;
        }

        let after_key = &remaining[pos + key.len()..];

        let quote_len = if after_key.starts_with('"') || after_key.starts_with('\'') {
            1
        } else {
            0
        };
        let after_key_quote = &after_key[quote_len..];

        let ws1_len = after_key_quote.len() - after_key_quote.trim_start().len();
        let after_ws1 = &after_key_quote[ws1_len..];

        if !after_ws1.starts_with(':') && !after_ws1.starts_with('=') {
            out.push_str(&remaining[..pos + key.len()]);
            remaining = &remaining[pos + key.len()..];
            continue;
        }

        let after_sep = &after_ws1[1..];
        let ws2_len = after_sep.len() - after_sep.trim_start().len();
        let after_ws2 = &after_sep[ws2_len..];

        let prefix_len = pos + key.len() + quote_len + ws1_len + 1 + ws2_len;
        out.push_str(&remaining[..prefix_len]);

        if let Some(q) = after_ws2.chars().next().filter(|&c| c == '"' || c == '\'') {
            out.push(q);
            let val_content = &after_ws2[q.len_utf8()..];
            let val_len = find_closing_quote(val_content, q).unwrap_or(val_content.len());
            let val = &val_content[..val_len];
            if val.trim().is_empty()
                || val == "[REDACTED]"
                || val.to_ascii_lowercase().starts_with("bearer [redacted]")
            {
                out.push_str(val);
            } else if val.to_ascii_lowercase().starts_with("bearer ") {
                let bearer_prefix = &val[..7];
                out.push_str(bearer_prefix);
                out.push_str("[REDACTED]");
            } else {
                out.push_str("[REDACTED]");
            }
            if val_len < val_content.len() {
                out.push(q);
                remaining = &val_content[val_len + q.len_utf8()..];
            } else {
                remaining = "";
            }
        } else {
            let lower = after_ws2.to_ascii_lowercase();
            if lower.starts_with("bearer [redacted]") {
                out.push_str(&after_ws2[..17]);
                remaining = &after_ws2[17..];
                continue;
            } else if lower.starts_with("bearer ") {
                let ws_bearer = 7 + (after_ws2[7..].len() - after_ws2[7..].trim_start().len());
                out.push_str(&after_ws2[..ws_bearer]);
                let rest = &after_ws2[ws_bearer..];
                let token_len = rest
                    .find(|c: char| {
                        c.is_whitespace()
                            || c == '&'
                            || c == ';'
                            || c == ','
                            || c == '}'
                            || c == ']'
                            || c == '"'
                            || c == '\''
                    })
                    .unwrap_or(rest.len());
                let token = &rest[..token_len];
                if token != "[REDACTED]" && !token.is_empty() {
                    out.push_str("[REDACTED]");
                } else {
                    out.push_str(token);
                }
                remaining = &rest[token_len..];
            } else {
                let val_len = after_ws2
                    .find(|c: char| {
                        c.is_whitespace()
                            || c == '&'
                            || c == ';'
                            || c == ','
                            || c == '}'
                            || c == ']'
                            || c == '"'
                            || c == '\''
                    })
                    .unwrap_or(after_ws2.len());

                let val = &after_ws2[..val_len];
                if val.trim().is_empty() || val == "[REDACTED]" {
                    out.push_str(val);
                } else {
                    out.push_str("[REDACTED]");
                }
                remaining = &after_ws2[val_len..];
            }
        }
    }

    out.push_str(remaining);
    out
}

fn find_closing_quote(s: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(i);
        }
    }
    None
}

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .char_indices()
        .find(|&(i, _)| {
            haystack[i..]
                .get(..needle.len())
                .is_some_and(|slice| slice.eq_ignore_ascii_case(needle))
        })
        .map(|(i, _)| i)
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
        let req = self.gate_images(req)?;
        let body = self.build_body(&req);
        let max_retries = self.defaults.max_retries;

        let body_keys: Vec<&str> = body
            .as_object()
            .map(|obj| obj.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let url_redacted = redact(&url);
        let model = &req.model;
        log::debug!("POST {url_redacted} model={model} keys={body_keys:?}");

        if log::log_enabled!(log::Level::Trace) {
            let serialized_body = serde_json::to_string(&body).unwrap_or_default();
            let body_redacted = redact(&vision::redact_data_urls(&serialized_body));
            log::trace!("request body: {body_redacted}");
        }

        let effort = req.effort.unwrap_or(self.defaults.effort);
        let opener = crate::retry::StreamOpener {
            client: self.client.clone(),
            url,
            api_key: self.api_key.clone(),
            headers: self
                .cfg
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            body,
            model: req.model,
            stream_idle_secs: self.defaults.stream_idle_secs,
            effort,
            max_retries,
            backoff_base: self.backoff_base,
        };

        // The first open is eager: a hard failure (auth, 4xx) is reported by `complete`
        // itself rather than as the first item of an otherwise valid stream.
        let inner = opener.open().await?;
        Ok(crate::retry::retrying_stream(opener, inner))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if let Ok(guard) = self.catalog_cache.lock()
            && let Some(cached) = guard.as_ref()
        {
            return Ok(cached.clone());
        }

        self.fetch_models().await
    }

    async fn refresh_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.fetch_models().await
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
            .unwrap_or_else(|| self.compat.heuristic_reasoning(model));

        let pricing = self.pricing.get(model).copied();

        let mut info = ModelInfo {
            route: ModelRoute::new(self.id.clone(), model),
            display_name,
            context_window,
            supports_reasoning,
            vision: self.vision_for(model),
            pricing,
            kind: crate::model_meta::kind_from_id(model),
            ..ModelInfo::default()
        };
        crate::model_meta::finish(&mut info, self.compat.reasoning_wire(model));
        info
    }
}
