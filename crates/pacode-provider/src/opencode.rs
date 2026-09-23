//! OpenCode Zen free-tier integration.
//!
//! Fetches the model catalog from `https://models.opencode.ai/api.json`,
//! filters only free models (`cost.input == 0`), and provides the headers
//! required by the Zen API (`x-opencode-*`, `User-Agent`).

use pacode_types::{Effort, ModelInfo, ModelRoute, Pricing};
use rand::Rng;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::openai_compat::OpenAiCompat;
use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod opencode_tests;

pub const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
pub const OPENCODE_CATALOG_URL: &str = "https://models.opencode.ai/api.json";
/// Mimic the opencode CLI version for Zen API compatibility.
pub const OPENCODE_VERSION: &str = "1.18.29";

/// Fetch the OpenCode model catalog and return only free models.
pub async fn fetch_free_models(client: &reqwest::Client) -> Result<Vec<ModelInfo>, ProviderError> {
    let response = client
        .get(OPENCODE_CATALOG_URL)
        .header("User-Agent", format!("opencode/{OPENCODE_VERSION}"))
        .send()
        .await
        .map_err(|e| ProviderError::Transport(format!("catalog fetch failed: {e}")))?;

    let status = response.status().as_u16();
    if status != 200 {
        let text = response.text().await.unwrap_or_default();
        return Err(ProviderError::Http {
            status,
            message: crate::error_body_message(&text),
        });
    }

    let text = response
        .text()
        .await
        .map_err(|e| ProviderError::Transport(format!("reading catalog body: {e}")))?;

    let catalog: Value = serde_json::from_str(&text)
        .map_err(|e| ProviderError::Malformed(format!("catalog JSON parse: {e}")))?;

    parse_free_models(&catalog)
}

/// Extract free models from a pre-parsed catalog JSON.
pub fn parse_free_models(catalog: &Value) -> Result<Vec<ModelInfo>, ProviderError> {
    let provider = catalog
        .get("opencode")
        .ok_or_else(|| ProviderError::Malformed("opencode provider not found in catalog".into()))?;

    let models_obj = provider
        .get("models")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ProviderError::Malformed("opencode.models is not an object".into()))?;

    let mut models = Vec::new();
    for (_key, model) in models_obj {
        let cost = match model.get("cost") {
            Some(c) => c,
            None => continue,
        };
        let input_cost = cost.get("input").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let output_cost = cost.get("output").and_then(|v| v.as_f64()).unwrap_or(1.0);
        if input_cost != 0.0 || output_cost != 0.0 {
            continue;
        }

        if let Some(info) = map_model(model) {
            models.push(info);
        }
    }

    models.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(models)
}

fn map_model(model: &Value) -> Option<ModelInfo> {
    let id = model.get("id")?.as_str()?;
    let name = model.get("name").and_then(|v| v.as_str()).unwrap_or(id);

    let limit = model.get("limit");
    let context_window = limit
        .and_then(|l| l.get("context"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let supports_reasoning = model
        .get("reasoning")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let supports_vision = model
        .get("attachment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || model
            .get("modalities")
            .and_then(|m| m.get("input"))
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("image")));

    let supported_efforts = parse_efforts(model);

    let cache_read = model
        .get("cost")
        .and_then(|c| c.get("cache_read"))
        .and_then(|v| v.as_f64());

    Some(ModelInfo {
        route: ModelRoute::new("opencode", id),
        display_name: name.to_string(),
        context_window,
        supports_reasoning,
        supports_vision,
        supported_efforts,
        pricing: Some(Pricing {
            input_per_m: 0.0,
            output_per_m: 0.0,
            cache_read_per_m: cache_read,
            cache_write_per_m: None,
        }),
    })
}

fn parse_efforts(model: &Value) -> Option<Vec<Effort>> {
    let options = model.get("reasoning_options")?.as_array()?;
    for opt in options {
        if opt.get("type").and_then(|v| v.as_str()) == Some("effort") {
            let values = opt.get("values")?.as_array()?;
            let efforts: Vec<Effort> = values
                .iter()
                .filter_map(|v| v.as_str().and_then(Effort::parse))
                .collect();
            if !efforts.is_empty() {
                return Some(efforts);
            }
        }
    }
    None
}

/// Build opencode-specific HTTP headers for Zen API requests.
///
/// These headers identify the client to the OpenCode Zen server,
/// mimicking the opencode CLI to access the free tier.
pub fn opencode_headers(session_id: &str) -> Vec<(String, String)> {
    let request_id = random_hex_id();
    vec![
        (
            "User-Agent".to_string(),
            format!("opencode/{OPENCODE_VERSION}"),
        ),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), session_id.to_string()),
        ("x-opencode-request".to_string(), request_id),
    ]
}

/// Generate a random 32-char hex string (128 bits) for request IDs.
pub fn random_hex_id() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// OpenCodeProvider: wraps OpenAiCompat, overrides catalog with api.json
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use pacode_types::{ProviderConfig, ProviderDefaults};

pub struct OpenCodeProvider {
    inner: OpenAiCompat,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    client: reqwest::Client,
}

impl OpenCodeProvider {
    pub fn new(
        id: String,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: Option<String>,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let client = reqwest::Client::new();
        let inner = OpenAiCompat::new(id, cfg, defaults, api_key, pricing)?;
        Ok(Self {
            inner,
            catalog_cache: Mutex::new(None),
            client,
        })
    }
}

#[async_trait]
impl Provider for OpenCodeProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        self.inner.complete(req).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if let Ok(guard) = self.catalog_cache.lock()
            && let Some(cached) = guard.as_ref()
        {
            return Ok(cached.clone());
        }
        self.refresh_models().await
    }

    async fn refresh_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let models = fetch_free_models(&self.client).await?;
        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = Some(models.clone());
        }
        Ok(models)
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        // Check cached catalog first
        if let Ok(guard) = self.catalog_cache.lock()
            && let Some(cached) = guard.as_ref()
            && let Some(info) = cached.iter().find(|m| m.route.model == model)
        {
            return info.clone();
        }
        // Fallback: synthesize minimal info
        ModelInfo {
            route: ModelRoute::new("opencode", model),
            display_name: pacode_types::prettify_model_name(model),
            context_window: None,
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: Some(Pricing {
                input_per_m: 0.0,
                output_per_m: 0.0,
                cache_read_per_m: Some(0.0),
                cache_write_per_m: None,
            }),
        }
    }
}
