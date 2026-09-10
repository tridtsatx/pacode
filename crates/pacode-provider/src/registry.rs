//! `ProviderRegistry`: `provider/model` routes → provider instances.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pacode_types::{Config, ModelInfo, ModelRoute};

use crate::catalog_cache::{
    CatalogCache, DEFAULT_CATALOG_TTL_SECS, DEFAULT_MAX_MODELS_PER_PROVIDER,
};
use crate::openai_compat::OpenAiCompat;
use crate::{Provider, ProviderError};

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn Provider>>,
    default_route: Option<ModelRoute>,
    catalog_cache: Arc<CatalogCache>,
}

impl ProviderRegistry {
    pub fn empty() -> Self {
        Self {
            providers: BTreeMap::new(),
            default_route: None,
            catalog_cache: Arc::new(CatalogCache::new(
                None,
                DEFAULT_CATALOG_TTL_SECS,
                DEFAULT_MAX_MODELS_PER_PROVIDER,
            )),
        }
    }

    /// Build every `[providers.<id>]` as an `OpenAiCompat`. `api_keys` are resolved by
    /// the caller (`pacode_config::resolve_api_key`). A provider with an empty
    /// `base_url` is an error.
    pub fn from_config(
        cfg: &Config,
        api_keys: &BTreeMap<String, Option<String>>,
    ) -> Result<Self, ProviderError> {
        let mut registry = Self::empty();
        let cache_path = pacode_config::Paths::discover().catalog_cache_file();
        registry.catalog_cache = Arc::new(CatalogCache::new(
            Some(cache_path),
            cfg.provider.catalog_ttl_secs,
            DEFAULT_MAX_MODELS_PER_PROVIDER,
        ));
        for (id, pcfg) in &cfg.providers {
            if pcfg.base_url.trim().is_empty() {
                return Err(ProviderError::Config(format!(
                    "provider '{id}' has empty base_url"
                )));
            }
            let api_key = api_keys.get(id).cloned().flatten();
            let provider = OpenAiCompat::new(
                id.clone(),
                pcfg.clone(),
                cfg.provider.clone(),
                api_key,
                cfg.pricing.clone(),
            )?;
            registry.insert(Arc::new(provider));
        }
        registry.set_default_route(cfg.default_route());
        Ok(registry)
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
        self.providers.insert(provider.id().to_string(), provider);
    }

    pub fn set_default_route(&mut self, route: Option<ModelRoute>) {
        self.default_route = route;
    }

    pub fn default_route(&self) -> Option<&ModelRoute> {
        self.default_route.as_ref()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }

    pub fn resolve(&self, route: &ModelRoute) -> Result<Arc<dyn Provider>, ProviderError> {
        self.get(&route.provider)
            .ok_or_else(|| ProviderError::Config(format!("unknown provider '{}'", route.provider)))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Parse user input (`/model`, CLI, config) into a route.
    pub fn parse_route(&self, s: &str) -> Option<ModelRoute> {
        let fallback = self
            .default_route
            .as_ref()
            .map(|r| r.provider.as_str())
            .or_else(|| self.providers.keys().next().map(String::as_str));
        ModelRoute::parse(s, self.ids(), fallback)
    }

    /// Catalog of every provider (cached immediately; refreshed in background when stale).
    pub async fn list_all_models(&self) -> Vec<ModelInfo> {
        let mut all_models = Vec::new();
        for (id, provider) in &self.providers {
            let models = self
                .catalog_cache
                .get_models_for_provider(id, provider)
                .await;
            all_models.extend(models);
        }
        all_models
    }
}
