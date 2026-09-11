//! Devin (devin.ai / Cognition) transport over Connect-RPC with protobuf codec.
//!
//! Protocol flow:
//! 1. `list_models`: calls `/exa.api_server_pb.ApiServerService/GetCliModelConfigs`
//!    encoded as `{1 metadata}` and returns repeated model configurations. Merges
//!    returned models with statically configured ones.
//! 2. `complete`: calls `/exa.api_server_pb.ApiServerService/GetChatMessage` server stream
//!    directly (model selected in request field 21). Decodes protobuf frames and detects
//!    inline tool calls within `delta_text`.
//! 3. Retries and idle timeouts reuse the crate's backoff and timeout machinery.

pub mod connect;
pub mod proto;
pub mod stream;
pub mod tool_parser;
pub mod wire;

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{
    ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, Role, prettify_model_name,
};

pub use connect::{
    CONNECT_FLAG_END_STREAM, ConnectClient, ConnectFrameDecoder, DecodedFrame, encode_connect_frame,
};
pub use proto::{ProtoError, Reader, WireType, WireValue, Writer, decode_varint, encode_varint};
pub use stream::{DevinStreamOpener, create_devin_event_stream, devin_retrying_stream};
pub use tool_parser::{InlineToolCallParser, find_json_object_end};
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

/// Devin provider implementation communicating via Connect-RPC with protobuf payloads.
pub struct Devin {
    id: String,
    cfg: ProviderConfig,
    defaults: ProviderDefaults,
    api_key: String,
    session_token: String,
    api_server_url: String,
    pricing: BTreeMap<String, Pricing>,
    connect_client: ConnectClient,
    catalog_cache: Mutex<Option<Vec<ModelInfo>>>,
    backoff_base: Duration,
}

impl Devin {
    /// Construct a new Devin provider instance with both api_key and session_token.
    pub fn new(
        id: impl Into<String>,
        cfg: ProviderConfig,
        defaults: ProviderDefaults,
        api_key: impl IntoOptionString,
        session_token: impl IntoOptionString,
        api_server_url: impl IntoOptionString,
        pricing: BTreeMap<String, Pricing>,
    ) -> Result<Self, ProviderError> {
        let id = id.into();
        let api_key = api_key.into_option_string().unwrap_or_default();
        let session_token = session_token.into_option_string().unwrap_or_default();
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
            session_token.clone(),
            custom_headers,
        );

        Ok(Self {
            id,
            cfg,
            defaults,
            api_key,
            session_token,
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

    pub fn session_token(&self) -> &str {
        &self.session_token
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

        let req_bytes = encode_get_cli_model_configs_request(&self.session_token);
        let rpc_result = self
            .connect_client
            .unary(
                "/exa.api_server_pb.ApiServerService/GetCliModelConfigs",
                &req_bytes,
            )
            .await;

        let resp_bytes = match rpc_result {
            Ok(b) => b,
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

        let response = match decode_get_cli_model_configs_response(&resp_bytes) {
            Ok(r) => r,
            Err(err) => {
                if !configured_models.is_empty() {
                    let url_redacted = redact(&self.api_server_url);
                    log::warn!("failed to parse Devin models proto from {url_redacted}: {err}");
                    if let Ok(mut guard) = self.catalog_cache.lock() {
                        *guard = Some(configured_models.clone());
                    }
                    return Ok(configured_models);
                }
                return Err(ProviderError::Malformed(format!(
                    "failed to parse GetCliModelConfigs response: {err}"
                )));
            }
        };

        let mut all_models = configured_models;
        for model_cfg in response.models {
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
                    info.context_window = Some(cw as u32);
                }
                all_models.push(info);
            }
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

    async fn complete(&self, mut req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let effort = req.effort.unwrap_or(self.defaults.effort);

        let assignment_jwt = if req.model.eq_ignore_ascii_case("adaptive") {
            // Must be the same id the chat request sends in field 16: the assignment
            // token is bound to the conversation.
            let conversation_id = crate::devin::wire::conversation_id(&req);
            let latest_user_msg = req.messages.iter().rev().find(|m| m.role == Role::User);
            let req_bytes = encode_assign_model_request(
                &self.session_token,
                "adaptive",
                &conversation_id,
                latest_user_msg,
            );
            let resp_bytes = self
                .connect_client
                .unary(
                    "/exa.api_server_pb.ApiServerService/AssignModel",
                    &req_bytes,
                )
                .await?;
            let resp = decode_assign_model_response(&resp_bytes).map_err(|e| {
                ProviderError::Malformed(format!("failed to parse AssignModel response: {e}"))
            })?;
            let assignment = resp.assignment.ok_or_else(|| {
                ProviderError::Malformed(
                    "AssignModel response missing assignment field".to_string(),
                )
            })?;
            req.model = assignment.assigned_model_uid;
            Some(assignment.assignment_jwt)
        } else {
            None
        };

        let opener = DevinStreamOpener {
            connect_client: self.connect_client.clone(),
            session_token: self.session_token.clone(),
            req,
            defaults: self.defaults.clone(),
            cfg: self.cfg.clone(),
            stream_idle_secs: self.defaults.stream_idle_secs,
            effort,
            max_retries: self.defaults.max_retries,
            backoff_base: self.backoff_base,
            assignment_jwt,
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
