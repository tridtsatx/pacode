//! OpenAI Responses API transport (Codex CLI over ChatGPT OAuth or API key).
//!
//! Handles:
//! - ChatGPT backend `https://chatgpt.com/backend-api/codex/responses` for OAuth, or
//!   `https://api.openai.com/v1/responses` for plain API key.
//! - Headers: `Authorization: Bearer <token>`, `OpenAI-Beta: responses=experimental`,
//!   and for OAuth: `originator: codex_cli_rs`, `chatgpt-account-id: <id>`.
//! - Responses request format: `instructions` system prompt, `input` items, Responses
//!   `tools` schema, `reasoning: {effort, summary: "auto"}`.
//! - Streaming SSE decode: `response.output_text.delta`, `response.reasoning_summary_text.delta`,
//!   `response.output_item.added/done` with tool calls, `response.completed` with usage.

mod auth;
mod events;
mod request;
mod stream;

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{
    ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, prettify_model_name,
};
use serde_json::Value;

pub use auth::{CodexAuth, CodexOAuthTokens, decode_base64_url, extract_account_id};
pub use events::{ResponsesChunkState, extract_responses_error, responses_chunk_to_events};
pub use request::{
    REASONING_KEYWORDS, build_responses_body, deep_merge, model_heuristic_reasoning,
    reasoning_enabled_for,
};
pub use stream::{CodexStreamOpener, create_responses_event_stream, retrying_responses_stream};

use crate::{CompletionRequest, EventStream, Provider, ProviderError, redact};

#[cfg(test)]
#[path = "codex_tests.rs"]
mod codex_tests;

pub const CHATGPT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
pub const CHATGPT_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const OPENAI_BETA_HEADER_VALUE: &str = "responses=experimental";

pub struct Codex {
    id: String,
    cfg: ProviderConfig,
    defaults: ProviderDefaults,
    auth: Option<CodexAuth>,
    pricing: BTreeMap<String, Pricing>,
    client: reqwest::Client,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    backoff_base: Duration,
}

impl Codex {
    /// Create a new Codex provider instance.
    pub fn new(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        auth: Option<CodexAuth>,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let id = id.into();
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

        Ok(Self {
            id,
            cfg,
            defaults,
            auth,
            pricing,
            client,
            catalog_cache: Mutex::new(None),
            backoff_base: crate::retry::DEFAULT_BACKOFF_BASE,
        })
    }

    /// Convenience constructor from an optional API key string.
    pub fn from_api_key(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: Option<String>,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let auth = api_key
            .filter(|k| !k.trim().is_empty())
            .map(CodexAuth::api_key);
        Self::new(id, cfg, defaults, auth, pricing)
    }

    #[cfg(test)]
    pub(crate) fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// `true` if configured with a ChatGPT OAuth credential.
    pub fn is_oauth(&self) -> bool {
        self.auth.as_ref().is_some_and(CodexAuth::is_oauth)
    }

    /// Target endpoint for completions: ChatGPT backend for OAuth, OpenAI platform for API keys.
    pub fn endpoint(&self) -> String {
        if self.is_oauth() {
            CHATGPT_RESPONSES_URL.to_string()
        } else {
            let base = self.cfg.base_url.trim();
            if base.is_empty() || base == "https://api.openai.com/v1" {
                OPENAI_RESPONSES_URL.to_string()
            } else {
                let trimmed = base
                    .trim_end_matches('/')
                    .trim_end_matches("/responses")
                    .trim_end_matches('/');
                format!("{trimmed}/responses")
            }
        }
    }

    /// Headers to attach to the Responses API request.
    pub fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        headers.push((
            "OpenAI-Beta".to_string(),
            OPENAI_BETA_HEADER_VALUE.to_string(),
        ));

        if let Some(auth) = &self.auth {
            let token = auth.bearer_token();
            if !token.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }
            if auth.is_oauth() {
                headers.push(("originator".to_string(), ORIGINATOR.to_string()));
                if let Some(account_id) = auth.account_id() {
                    headers.push(("chatgpt-account-id".to_string(), account_id));
                }
            }
        }

        for (k, v) in &self.cfg.headers {
            headers.push((k.clone(), v.clone()));
        }

        headers
    }

    /// Build the JSON body for `req` in OpenAI Responses format.
    pub fn build_body(&self, req: &CompletionRequest) -> Value {
        build_responses_body(&self.cfg, &self.defaults, self.is_oauth(), req)
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

        let url = if self.is_oauth() {
            CHATGPT_MODELS_URL.to_string()
        } else {
            let base = self
                .cfg
                .base_url
                .trim_end_matches('/')
                .trim_end_matches("/responses")
                .trim_end_matches('/');
            format!("{base}/models")
        };

        let mut req_builder = self.client.get(&url);
        if let Some(auth) = &self.auth {
            let token = auth.bearer_token();
            if !token.is_empty() {
                req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
            }
            if auth.is_oauth() {
                req_builder = req_builder.header("originator", ORIGINATOR);
                if let Some(account_id) = auth.account_id() {
                    req_builder = req_builder.header("chatgpt-account-id", account_id);
                }
            }
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
        let model_entries = json_val
            .get("models")
            .and_then(|v| v.as_array())
            .or_else(|| json_val.get("data").and_then(|v| v.as_array()));

        if let Some(items) = model_entries {
            for item in items {
                let id_opt = item
                    .get("id")
                    .or_else(|| item.get("slug"))
                    .and_then(|v| v.as_str());
                if let Some(id) = id_opt
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
}

#[async_trait]
impl Provider for Codex {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let url = self.endpoint();
        let headers = self.build_headers();
        let body = self.build_body(&req);
        let max_retries = self.defaults.max_retries;

        let body_keys: Vec<&str> = body
            .as_object()
            .map(|obj| obj.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let url_redacted = redact(&url);
        let model = &req.model;
        log::debug!("POST {url_redacted} model={model} keys={body_keys:?}");

        let serialized_body = serde_json::to_string(&body).unwrap_or_default();
        let body_redacted = redact(&serialized_body);
        log::trace!("request body: {body_redacted}");

        let effort = req.effort.unwrap_or(self.defaults.effort);
        let opener = CodexStreamOpener {
            client: self.client.clone(),
            url,
            headers,
            body,
            model: req.model,
            stream_idle_secs: self.defaults.stream_idle_secs,
            effort,
            max_retries,
            backoff_base: self.backoff_base,
        };

        let inner = opener.open().await?;
        Ok(retrying_responses_stream(opener, inner))
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
