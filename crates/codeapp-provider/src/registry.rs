//! `ProviderRegistry`: `provider/model` routes → provider instances.

use std::collections::BTreeMap;
use std::sync::Arc;

use codeapp_types::{Config, ModelInfo, ModelRoute};

use crate::{Provider, ProviderError};

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn Provider>>,
    default_route: Option<ModelRoute>,
}

impl ProviderRegistry {
    pub fn empty() -> Self {
        Self {
            providers: BTreeMap::new(),
            default_route: None,
        }
    }

    /// Build every `[providers.<id>]` as an `OpenAiCompat`. `api_keys` are resolved by
    /// the caller (`codeapp_config::resolve_api_key`). A provider with an empty
    /// `base_url` is an error.
    pub fn from_config(
        cfg: &Config,
        api_keys: &BTreeMap<String, Option<String>>,
    ) -> Result<Self, ProviderError> {
        let _ = (cfg, api_keys);
        todo!("ProviderRegistry::from_config")
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

    /// Catalog of every provider (errors of one provider are logged and skipped).
    pub async fn list_all_models(&self) -> Vec<ModelInfo> {
        todo!("ProviderRegistry::list_all_models")
    }
}
