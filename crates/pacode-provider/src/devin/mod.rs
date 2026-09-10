//! Devin (devin.ai / Cognition) transport over Connect-RPC.
//!
//! Protocol flow:
//! 1. `list_models`: calls `/exa.api_server_pb.ApiServerService/GetCliModelConfigs`
//!    returning `GetCliModelConfigsResponse`. Merges returned model configs with
//!    statically configured models.
//! 2. `complete`: first calls `/exa.api_server_pb.ApiServerService/AssignModel`
//!    to retrieve an `assignment_jwt`, then opens a Connect server stream
//!    `/exa.api_server_pb.ApiServerService/GetChatMessage` carrying that JWT.
//!    Decodes `GetChatMessageResponse` frames into [`StreamEvent`] items.
//! 3. Retries and idle timeouts reuse the crate's backoff and timeout machinery.

pub mod connect;
pub mod stream;
pub mod wire;

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{
    ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, prettify_model_name,
};

pub use connect::{
    CONNECT_FLAG_END_STREAM, ConnectClient, ConnectFrameDecoder, DecodedFrame, encode_connect_frame,
};
pub use stream::{DevinStreamOpener, create_devin_event_stream, devin_retrying_stream};
pub use wire::*;

use crate::{CompletionRequest, EventStream, Provider, ProviderError, redact};

#[cfg(test)]
#[path = "../devin_tests.rs"]
mod devin_tests;

/// Trait allowing [`Devin::new`] to accept `String`, `&str`, `Option<String>`, etc.
pub trait IntoOptionString {
    fn into_option_string(self) -> Option<String>;
}

impl IntoOptionString for Option<String> {
    fn into_option_string(self) -> Option<String> {
        self
    }
}

impl IntoOptionString for Option<&str> {
    fn into_option_string(self) -> Option<String> {
        self.map(|s| s.to_string())
    }
}

impl IntoOptionString for String {
    fn into_option_string(self) -> Option<String> {
        Some(self)
    }
}

impl IntoOptionString for &str {
    fn into_option_string(self) -> Option<String> {
        Some(self.to_string())
    }
}

/// Devin provider implementation communicating via Connect-RPC.
pub struct Devin {
    id: String,
    cfg: ProviderConfig,
    defaults: ProviderDefaults,
    api_key: String,
    api_server_url: String,
    pricing: BTreeMap<String, Pricing>,
    connect_client: ConnectClient,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    backoff_base: Duration,
}

impl Devin {
    /// Construct a new Devin provider instance.
    pub fn new(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: impl IntoOptionString,
        api_server_url: impl IntoOptionString,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let id = id.into();
        let api_key = api_key.into_option_string().unwrap_or_default();
        let api_server_url = api_server_url
            .into_option_string()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| cfg.base_url.clone());

        if api_server_url.trim().is_empty() {
            return Err(ProviderError::Config(format!(
                "provider '{id}' has empty api_server_url"
            )));
        }

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::Config(format!("failed to build HTTP client: {e}")))?;

        let custom_headers: Vec<(String, String)> = cfg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let connect_client = ConnectClient::new(
            client,
            api_server_url.clone(),
            api_key.clone(),
            custom_headers,
        );

        Ok(Self {
            id,
            cfg,
            defaults,
            api_key,
            api_server_url,
            pricing,
            connect_client,
            catalog_cache: Mutex::new(None),
            backoff_base: crate::retry::DEFAULT_BACKOFF_BASE,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    pub fn api_server_url(&self) -> &str {
        &self.api_server_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn connect_client(&self) -> &ConnectClient {
        &self.connect_client
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

        let req = GetCliModelConfigsRequest::to_wire();
        let rpc_result = self
            .connect_client
            .unary::<_, GetCliModelConfigsResponse>(
                "/exa.api_server_pb.ApiServerService/GetCliModelConfigs",
                &req,
            )
            .await;

        let response = match rpc_result {
            Ok(resp) => resp,
            Err(err) => {
                if !configured_models.is_empty() {
                    let url_redacted = redact(&self.api_server_url);
                    log::warn!("failed to fetch Devin models from {url_redacted}: {err}");
                    if let Ok(mut guard) = self.catalog_cache.lock() {
                        *guard = Some(configured_models.clone());
                    }
                    return Ok(configured_models);
                }
                return Err(err);
            }
        };

        let mut all_models = configured_models;
        for model_cfg in response.client_model_configs {
            if let Some(uid) = model_cfg.model_uid
                && !uid.is_empty()
                && seen.insert(uid.clone())
            {
                let mut info = self.model_info(&uid);
                if let Some(dn) = model_cfg.display_name
                    && !dn.is_empty()
                {
                    info.display_name = dn;
                }
                if let Some(cw) = model_cfg.context_window {
                    info.context_window = Some(cw);
                }
                if let Some(r) = model_cfg.supports_reasoning {
                    info.supports_reasoning = r;
                }
                all_models.push(info);
            }
        }

        if let Some(sub_uid) = response.subagent_default_model_uid
            && !sub_uid.is_empty()
            && seen.insert(sub_uid.clone())
        {
            all_models.push(self.model_info(&sub_uid));
        }

        if let Some(ov_cfg) = response.default_override_model_config
            && let Some(uid) = ov_cfg.model_uid
            && !uid.is_empty()
            && seen.insert(uid.clone())
        {
            let mut info = self.model_info(&uid);
            if let Some(dn) = ov_cfg.display_name
                && !dn.is_empty()
            {
                info.display_name = dn;
            }
            if let Some(cw) = ov_cfg.context_window {
                info.context_window = Some(cw);
            }
            if let Some(r) = ov_cfg.supports_reasoning {
                info.supports_reasoning = r;
            }
            all_models.push(info);
        }

        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = Some(all_models.clone());
        }

        Ok(all_models)
    }
}

#[async_trait]
impl Provider for Devin {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let effort = req.effort.unwrap_or(self.defaults.effort);
        let opener = DevinStreamOpener {
            connect_client: self.connect_client.clone(),
            req,
            defaults: self.defaults.clone(),
            cfg: self.cfg.clone(),
            stream_idle_secs: self.defaults.stream_idle_secs,
            effort,
            max_retries: self.defaults.max_retries,
            backoff_base: self.backoff_base,
        };

        let inner = opener.open().await?;
        Ok(devin_retrying_stream(opener, inner))
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
            .or(Some(128_000));

        let supports_reasoning = model_cfg
            .and_then(|m| m.reasoning)
            .or(self.cfg.reasoning)
            .unwrap_or(true);

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
