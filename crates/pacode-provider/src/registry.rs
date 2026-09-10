//! `ProviderRegistry`: `provider/model` routes → provider instances.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use pacode_auth::store::{Account, AuthStore};
use pacode_types::{
    Config, ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, ProviderKind,
};

use crate::catalog_cache::{
    CatalogCache, DEFAULT_CATALOG_TTL_SECS, DEFAULT_MAX_MODELS_PER_PROVIDER,
};
use crate::openai_compat::OpenAiCompat;
use crate::{Provider, ProviderError};

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;

pub type TokenRefresher = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedCredential {
    ConfigApiKey(String),
    EnvApiKey(String),
    StoreOAuth(Account),
    None,
}

impl ResolvedCredential {
    pub fn is_from_store(&self) -> bool {
        matches!(self, Self::StoreOAuth(_))
    }

    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ConfigApiKey(k) | Self::EnvApiKey(k) => Some(k.as_str()),
            Self::StoreOAuth(acc) => Some(acc.access.as_str()),
            Self::None => None,
        }
    }
}

/// Resolve credential in precedence order:
/// 1. explicit `api_key` in config
/// 2. `api_key_env` (from pre-resolved `env_keys` or environment)
/// 3. credential store (`pacode-auth`)
pub fn resolve_provider_credential(
    id: &str,
    pcfg: &ProviderConfig,
    env_keys: &BTreeMap<String, Option<String>>,
    store: Option<&AuthStore>,
) -> ResolvedCredential {
    // 1. Explicit api_key in config
    if let Some(key) = &pcfg.api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return ResolvedCredential::ConfigApiKey(trimmed.to_string());
        }
    }

    // 2. api_key_env: check resolved env_keys first, then std::env::var if env var name configured
    if let Some(env_val) = env_keys.get(id).cloned().flatten() {
        let trimmed = env_val.trim();
        if !trimmed.is_empty() {
            return ResolvedCredential::EnvApiKey(trimmed.to_string());
        }
    }
    if let Some(env_var) = &pcfg.api_key_env
        && let Ok(val) = std::env::var(env_var)
    {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return ResolvedCredential::EnvApiKey(trimmed.to_string());
        }
    }

    // 3. Credential store (pacode-auth)
    if let Some(store) = store {
        // Direct id lookup
        if let Some(acc) = store.get(id) {
            return ResolvedCredential::StoreOAuth(acc.clone());
        }
        // Catalog lookup for aliases (e.g. claude -> anthropic)
        if let Some(desc) = pacode_auth::catalog::find(id)
            && let Some(acc) = store.get(desc.id)
        {
            return ResolvedCredential::StoreOAuth(acc.clone());
        }
    }

    ResolvedCredential::None
}

/// Synthesize built-in provider entries so `/login claude` etc. work with an empty config.toml.
///
/// Rules:
/// - claude -> kind anthropic, base_url `https://api.anthropic.com/v1`
/// - openai -> kind codex, base_url `https://api.openai.com/v1`
/// - devin  -> kind devin, base_url from account's `extra["api_server_url"]`
pub fn synthesize_builtin_providers(
    providers: &mut BTreeMap<String, ProviderConfig>,
    store: &AuthStore,
) {
    if !providers.contains_key("claude") && store.get("claude").is_some() {
        providers.insert(
            "claude".to_string(),
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".to_string(),
                catalog: true,
                ..ProviderConfig::default()
            },
        );
    }

    if !providers.contains_key("openai") && store.get("openai").is_some() {
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                kind: ProviderKind::Codex,
                base_url: "https://api.openai.com/v1".to_string(),
                catalog: true,
                ..ProviderConfig::default()
            },
        );
    }

    if !providers.contains_key("devin")
        && let Some(acc) = store.get("devin")
    {
        let base_url = acc
            .extra
            .get("api_server_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        providers.insert(
            "devin".to_string(),
            ProviderConfig {
                kind: ProviderKind::Devin,
                base_url,
                catalog: true,
                ..ProviderConfig::default()
            },
        );
    }
}

fn build_transport(
    id: &str,
    pcfg: &ProviderConfig,
    defaults: &ProviderDefaults,
    pricing: &BTreeMap<String, Pricing>,
    credential: &ResolvedCredential,
) -> Result<Arc<dyn Provider>, ProviderError> {
    match pcfg.kind {
        ProviderKind::OpenAi => {
            let api_key = credential.api_key().map(String::from);
            let provider = OpenAiCompat::new(
                id.to_string(),
                pcfg.clone(),
                defaults.clone(),
                api_key,
                pricing.clone(),
            )?;
            Ok(Arc::new(provider))
        }
        ProviderKind::Anthropic => match credential {
            ResolvedCredential::StoreOAuth(acc) => {
                let provider = crate::anthropic::Anthropic::from_oauth(
                    id.to_string(),
                    pcfg.clone(),
                    defaults.clone(),
                    acc.access.clone(),
                    pricing.clone(),
                )?;
                Ok(Arc::new(provider))
            }
            _ => {
                let api_key = credential.api_key().map(String::from);
                let provider = crate::anthropic::Anthropic::from_api_key(
                    id.to_string(),
                    pcfg.clone(),
                    defaults.clone(),
                    api_key,
                    pricing.clone(),
                )?;
                Ok(Arc::new(provider))
            }
        },
        ProviderKind::Codex => match credential {
            ResolvedCredential::StoreOAuth(acc) => {
                let tokens = crate::codex::CodexOAuthTokens {
                    access_token: acc.access.clone(),
                    refresh_token: acc.refresh.clone(),
                    id_token: acc
                        .extra
                        .get("id_token")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    account_id: acc
                        .extra
                        .get("account_id")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                };
                let auth = crate::codex::CodexAuth::oauth_tokens(tokens);
                let provider = crate::codex::Codex::new(
                    id.to_string(),
                    pcfg.clone(),
                    defaults.clone(),
                    Some(auth),
                    pricing.clone(),
                )?;
                Ok(Arc::new(provider))
            }
            _ => {
                let api_key = credential.api_key().map(String::from);
                let provider = crate::codex::Codex::from_api_key(
                    id.to_string(),
                    pcfg.clone(),
                    defaults.clone(),
                    api_key,
                    pricing.clone(),
                )?;
                Ok(Arc::new(provider))
            }
        },
        ProviderKind::Devin => {
            // Devin transport module is guarded because it is not yet available in the crate.
            Err(ProviderError::Config(format!(
                "provider '{id}' has kind 'devin', but the devin transport is not yet available"
            )))
        }
    }
}

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<BTreeMap<String, Arc<dyn Provider>>>>,
    default_route: Option<ModelRoute>,
    catalog_cache: Arc<CatalogCache>,
    configs: Arc<RwLock<BTreeMap<String, ProviderConfig>>>,
    defaults: Arc<RwLock<ProviderDefaults>>,
    pricing: Arc<RwLock<BTreeMap<String, Pricing>>>,
    store_providers: Arc<RwLock<BTreeMap<String, String>>>,
    token_refresher: Option<TokenRefresher>,
}

impl ProviderRegistry {
    pub fn empty() -> Self {
        Self {
            providers: Arc::new(RwLock::new(BTreeMap::new())),
            default_route: None,
            catalog_cache: Arc::new(CatalogCache::new(
                None,
                DEFAULT_CATALOG_TTL_SECS,
                DEFAULT_MAX_MODELS_PER_PROVIDER,
            )),
            configs: Arc::new(RwLock::new(BTreeMap::new())),
            defaults: Arc::new(RwLock::new(ProviderDefaults::default())),
            pricing: Arc::new(RwLock::new(BTreeMap::new())),
            store_providers: Arc::new(RwLock::new(BTreeMap::new())),
            token_refresher: None,
        }
    }

    /// Build providers from config and resolved API keys, consulting `AuthStore::load()` if present.
    pub fn from_config(
        cfg: &Config,
        api_keys: &BTreeMap<String, Option<String>>,
    ) -> Result<Self, ProviderError> {
        let store = AuthStore::load().ok();
        Self::from_config_with_store(cfg, api_keys, store.as_ref())
    }

    /// Build providers from config, resolved API keys, and an optional credential store.
    pub fn from_config_with_store(
        cfg: &Config,
        api_keys: &BTreeMap<String, Option<String>>,
        store: Option<&AuthStore>,
    ) -> Result<Self, ProviderError> {
        let mut registry = Self::empty();
        let cache_path = pacode_config::Paths::discover().catalog_cache_file();
        registry.catalog_cache = Arc::new(CatalogCache::new(
            Some(cache_path),
            cfg.provider.catalog_ttl_secs,
            DEFAULT_MAX_MODELS_PER_PROVIDER,
        ));

        let mut all_providers = cfg.providers.clone();
        if let Some(store) = store {
            synthesize_builtin_providers(&mut all_providers, store);
        }

        let mut store_providers = BTreeMap::new();
        let mut configs = BTreeMap::new();

        for (id, mut pcfg) in all_providers {
            let credential = resolve_provider_credential(&id, &pcfg, api_keys, store);

            // Devin api_server_url from extra if base_url is empty
            if pcfg.kind == ProviderKind::Devin
                && pcfg.base_url.trim().is_empty()
                && let ResolvedCredential::StoreOAuth(acc) = &credential
                && let Some(url) = acc.extra.get("api_server_url").and_then(|v| v.as_str())
            {
                pcfg.base_url = url.to_string();
            }

            if pcfg.base_url.trim().is_empty() && pcfg.kind != ProviderKind::Codex {
                return Err(ProviderError::Config(format!(
                    "provider '{id}' has empty base_url"
                )));
            }

            if let ResolvedCredential::StoreOAuth(acc) = &credential {
                store_providers.insert(id.clone(), acc.access.clone());
            }

            configs.insert(id.clone(), pcfg.clone());

            match build_transport(&id, &pcfg, &cfg.provider, &cfg.pricing, &credential) {
                Ok(provider) => {
                    registry.insert(provider);
                }
                Err(e) => {
                    // Guarded Devin: if explicitly configured, return error; if synthesized, skip
                    if cfg.providers.contains_key(&id) {
                        return Err(e);
                    }
                }
            }
        }

        *registry.configs.write().unwrap_or_else(|p| p.into_inner()) = configs;
        *registry
            .store_providers
            .write()
            .unwrap_or_else(|p| p.into_inner()) = store_providers;
        *registry.defaults.write().unwrap_or_else(|p| p.into_inner()) = cfg.provider.clone();
        *registry.pricing.write().unwrap_or_else(|p| p.into_inner()) = cfg.pricing.clone();
        registry.set_default_route(cfg.default_route());
        Ok(registry)
    }

    pub fn set_token_refresher(&mut self, refresher: TokenRefresher) {
        self.token_refresher = Some(refresher);
    }

    pub fn with_catalog_cache(mut self, cache: Arc<CatalogCache>) -> Self {
        self.catalog_cache = cache;
        self
    }

    pub fn catalog_cache(&self) -> Arc<CatalogCache> {
        Arc::clone(&self.catalog_cache)
    }

    pub fn set_cache_path(&self, path: PathBuf) {
        self.catalog_cache.set_cache_path(path);
    }

    pub fn set_ttl_secs(&self, ttl: u64) {
        self.catalog_cache.set_ttl_secs(ttl);
    }

    /// Warm the model catalog in the background.
    pub async fn prefetch(&self) {
        let _ = self.list_all_models().await;
    }

    pub fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(provider.id().to_string(), provider);
    }

    pub fn set_default_route(&mut self, route: Option<ModelRoute>) {
        self.default_route = route;
    }

    pub fn default_route(&self) -> Option<&ModelRoute> {
        self.default_route.as_ref()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }

    /// Resolve route to provider. Before a turn uses a provider whose credential
    /// came from the store, `access_token` is consulted so an expired token is refreshed.
    pub async fn resolve(&self, route: &ModelRoute) -> Result<Arc<dyn Provider>, ProviderError> {
        let is_store = {
            let guard = self
                .store_providers
                .read()
                .unwrap_or_else(|p| p.into_inner());
            guard.contains_key(&route.provider)
        };

        if is_store {
            let fresh_token = if let Some(refresher) = &self.token_refresher {
                match refresher(&route.provider).await {
                    Ok(token) => Some(token),
                    Err(e) => return Err(e),
                }
            } else {
                // No test seam installed: go through the auth crate so an expired
                // OAuth token is refreshed before the turn uses it. A provider whose
                // credential needs no refresh (api key, devin) returns it unchanged.
                match pacode_auth::flows::access_token(&route.provider).await {
                    Ok(token) => Some(token),
                    Err(e) => {
                        return Err(ProviderError::Config(format!(
                            "credentials for '{}' are unusable: {e}",
                            route.provider
                        )));
                    }
                }
            };

            if let Some(token) = fresh_token {
                let current_token = {
                    let guard = self
                        .store_providers
                        .read()
                        .unwrap_or_else(|p| p.into_inner());
                    guard.get(&route.provider).cloned()
                };

                if current_token.as_deref() != Some(&token) {
                    self.rebuild_provider_with_token(&route.provider, &token)?;
                }
            }
        }

        let prov_id = &route.provider;
        self.get(prov_id)
            .ok_or_else(|| ProviderError::Config(format!("unknown provider '{prov_id}'")))
    }

    pub fn rebuild_provider_with_token(
        &self,
        id: &str,
        new_token: &str,
    ) -> Result<(), ProviderError> {
        let pcfg = {
            let configs = self.configs.read().unwrap_or_else(|p| p.into_inner());
            configs.get(id).cloned()
        }
        .ok_or_else(|| ProviderError::Config(format!("provider '{id}' not found in registry")))?;

        let defaults = self
            .defaults
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let pricing = self
            .pricing
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();

        let account = Account {
            label: format!("{id}-active"),
            kind: "oauth".to_string(),
            access: new_token.to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        };
        let credential = ResolvedCredential::StoreOAuth(account);

        let provider = build_transport(id, &pcfg, &defaults, &pricing, &credential)?;
        self.providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_string(), provider);
        self.store_providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_string(), new_token.to_string());
        Ok(())
    }

    pub fn rebuild_provider_from_store(
        &self,
        id: &str,
        store: &AuthStore,
    ) -> Result<(), ProviderError> {
        let mut configs = self.configs.write().unwrap_or_else(|p| p.into_inner());
        if !configs.contains_key(id) && store.get(id).is_some() {
            synthesize_builtin_providers(&mut configs, store);
        }

        let Some(mut pcfg) = configs.get(id).cloned() else {
            self.providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(id);
            self.store_providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(id);
            return Ok(());
        };

        let defaults = self
            .defaults
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let pricing = self
            .pricing
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();

        let credential = resolve_provider_credential(id, &pcfg, &BTreeMap::new(), Some(store));
        if let ResolvedCredential::StoreOAuth(acc) = &credential {
            if pcfg.kind == ProviderKind::Devin
                && pcfg.base_url.trim().is_empty()
                && let Some(url) = acc.extra.get("api_server_url").and_then(|v| v.as_str())
            {
                pcfg.base_url = url.to_string();
            }
            self.store_providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id.to_string(), acc.access.clone());
        } else {
            self.store_providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(id);
            if matches!(id, "claude" | "openai" | "devin") && store.accounts(id).is_empty() {
                configs.remove(id);
                self.providers
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(id);
                return Ok(());
            }
        }

        match build_transport(id, &pcfg, &defaults, &pricing, &credential) {
            Ok(provider) => {
                self.providers
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.to_string(), provider);
                configs.insert(id.to_string(), pcfg);
                Ok(())
            }
            Err(e) => {
                if pcfg.kind == ProviderKind::Devin {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    pub fn ids(&self) -> Vec<String> {
        self.providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Parse user input (`/model`, CLI, config) into a route.
    pub fn parse_route(&self, s: &str) -> Option<ModelRoute> {
        let ids = self.ids();
        let fallback = self
            .default_route
            .as_ref()
            .map(|r| r.provider.as_str())
            .or_else(|| ids.first().map(String::as_str));
        ModelRoute::parse(s, ids.iter().map(String::as_str), fallback)
    }

    /// Catalog of every provider (cached immediately; refreshed in background when stale).
    pub async fn list_all_models(&self) -> Vec<ModelInfo> {
        let providers: Vec<(String, Arc<dyn Provider>)> = self
            .providers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect();
        let mut all_models = Vec::new();
        for (id, provider) in providers {
            let models = self
                .catalog_cache
                .get_models_for_provider(&id, &provider)
                .await;
            all_models.extend(models);
        }
        all_models
    }
}
