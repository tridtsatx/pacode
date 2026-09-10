//! Anthropic Messages API provider transport.
//!
//! Features:
//! - POST {base_url}/messages with `anthropic-version: 2023-06-01`, streaming SSE.
//! - Two auth modes: API key (`x-api-key` header) and OAuth access token (`Authorization: Bearer`).
//! - Full Claude Code OAuth compatibility contract:
//!   - `?beta=true` on the endpoint
//!   - `User-Agent: claude-cli/1.0.0`
//!   - `anthropic-beta: oauth-2025-04-20,claude-code-20250219`
//!   - Claude Code identity line as the first system block
//!   - Tool-name remapping table (`bash` <-> `Bash`, `read` <-> `Read`, ...)
//! - Anthropic SSE event decoding (message_start, content_block_start/delta/stop, message_delta, ping, error)
//! - Model catalog fetching (`GET /models`) and caching.

pub mod request;
pub mod retry;
pub mod stream;
pub mod types;

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{
    ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, prettify_model_name,
};
use serde_json::Value;

pub use types::{
    AnthropicAuth, CLAUDE_CLI_USER_AGENT, CLAUDE_CODE_IDENTITY, DEFAULT_MAX_OUTPUT_TOKENS,
    OAUTH_BETA, map_tool_name_for_oauth, map_tool_name_from_oauth,
};

use self::retry::{AnthropicStreamOpener, anthropic_retrying_stream};
use crate::{CompletionRequest, EventStream, Provider, ProviderError, redact};

#[cfg(test)]
#[path = "../anthropic_tests.rs"]
mod anthropic_tests;

pub struct Anthropic {
    id: String,
    cfg: ProviderConfig,
    defaults: ProviderDefaults,
    auth: Option<AnthropicAuth>,
    pricing: BTreeMap<String, Pricing>,
    client: reqwest::Client,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    backoff_base: Duration,
}

impl Anthropic {
    pub fn new(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        auth: Option<AnthropicAuth>,
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

    pub fn from_api_key(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: Option<String>,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let auth = api_key.map(AnthropicAuth::detect);
        Self::new(id, cfg, defaults, auth, pricing)
    }

    pub fn from_oauth(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        token: String,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        Self::new(
            id,
            cfg,
            defaults,
            Some(AnthropicAuth::OAuth(token)),
            pricing,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    pub fn is_oauth(&self) -> bool {
        if let Some(auth) = &self.auth {
            auth.is_oauth()
        } else {
            self.cfg
                .headers
                .get("anthropic-auth")
                .is_some_and(|v| v.eq_ignore_ascii_case("oauth"))
        }
    }

    pub fn auth(&self) -> Option<&AnthropicAuth> {
        self.auth.as_ref()
    }

    pub fn build_url(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        let mut url = if base.ends_with("/messages") {
            base.to_string()
        } else {
            format!("{base}/messages")
        };
        if self.is_oauth() {
            if url.contains('?') {
                url.push_str("&beta=true");
            } else {
                url.push_str("?beta=true");
            }
        }
        url
    }

    pub fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        headers.push((
            "anthropic-version".to_string(),
            types::ANTHROPIC_VERSION.to_string(),
        ));
        headers.push(("content-type".to_string(), "application/json".to_string()));
        headers.push(("accept".to_string(), "text/event-stream".to_string()));

        if self.is_oauth() {
            headers.push((
                "User-Agent".to_string(),
                types::CLAUDE_CLI_USER_AGENT.to_string(),
            ));
            headers.push(("anthropic-beta".to_string(), types::OAUTH_BETA.to_string()));
            if let Some(auth) = &self.auth {
                headers.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", auth.token()),
                ));
            }
        } else {
            headers.push((
                "User-Agent".to_string(),
                format!("pacode/{}", env!("CARGO_PKG_VERSION")),
            ));
            if let Some(auth) = &self.auth {
                headers.push(("x-api-key".to_string(), auth.token().to_string()));
            }
        }

        for (k, v) in &self.cfg.headers {
            if !k.eq_ignore_ascii_case("anthropic-auth") {
                headers.push((k.clone(), v.clone()));
            }
        }

        headers
    }

    pub fn build_body(&self, req: &CompletionRequest) -> Value {
        request::build_body(
            req,
            self.is_oauth(),
            self.reasoning_enabled_for(&req.model),
            self.defaults.effort,
            self.cfg.extra_body.as_ref(),
        )
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
        types::model_heuristic_reasoning(model)
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
        req_builder = req_builder.header("anthropic-version", types::ANTHROPIC_VERSION);

        if self.is_oauth() {
            req_builder = req_builder.header("User-Agent", types::CLAUDE_CLI_USER_AGENT);
            req_builder = req_builder.header("anthropic-beta", types::OAUTH_BETA);
            if let Some(auth) = &self.auth {
                req_builder =
                    req_builder.header("Authorization", format!("Bearer {}", auth.token()));
            }
        } else {
            req_builder = req_builder.header(
                "User-Agent",
                format!("pacode/{}", env!("CARGO_PKG_VERSION")),
            );
            if let Some(auth) = &self.auth {
                req_builder = req_builder.header("x-api-key", auth.token());
            }
        }

        for (k, v) in &self.cfg.headers {
            if !k.eq_ignore_ascii_case("anthropic-auth") {
                req_builder = req_builder.header(k, v);
            }
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
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let url = self.build_url();
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
        let opener = AnthropicStreamOpener {
            client: self.client.clone(),
            url,
            headers,
            body,
            model: req.model,
            stream_idle_secs: self.defaults.stream_idle_secs,
            effort,
            max_retries,
            backoff_base: self.backoff_base,
            is_oauth: self.is_oauth(),
        };

        let inner = opener.open().await?;
        Ok(anthropic_retrying_stream(opener, inner))
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
            .or(self.cfg.context_window)
            .unwrap_or(200_000);

        let supports_reasoning = model_cfg
            .and_then(|m| m.reasoning)
            .or(self.cfg.reasoning)
            .unwrap_or_else(|| types::model_heuristic_reasoning(model));

        let pricing = self.pricing.get(model).copied();

        ModelInfo {
            route: ModelRoute::new(self.id.clone(), model),
            display_name,
            context_window: Some(context_window),
            supports_reasoning,
            pricing,
        }
    }
}
