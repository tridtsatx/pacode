//! `ProviderRegistry`: `provider/model` routes → provider instances.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use pacode_auth::store::{Account, AuthStore};
use pacode_types::{
    Config, ModelInfo, ModelRoute, Pricing, ProviderConfig, ProviderDefaults, ProviderDialect,
    ProviderKind,
};

use crate::catalog_cache::{
    CatalogCache, DEFAULT_CATALOG_TTL_SECS, DEFAULT_MAX_MODELS_PER_PROVIDER,
};
use crate::dialect::{DialectContext, base_url_optional};
use crate::registry_cache::{TokenCache, legacy_resolve};
use crate::{Provider, ProviderError};

pub use crate::dialect::{DialectFactory, known_dialects};

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;

#[cfg(test)]
#[path = "declarative_tests.rs"]
mod declarative_tests;

/// The reported effect of a [`ProviderRegistry::reload`], so a caller can log
/// or surface what a config write actually changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadReport {
    /// Providers whose entry changed, so their transport was rebuilt.
    pub rebuilt: Vec<String>,
    /// Providers that appeared in the config.
    pub added: Vec<String>,
    /// Providers dropped from the registry because the config no longer has
    /// them.
    pub removed: Vec<String>,
    /// Providers left untouched (identical entry, same credential).
    pub unchanged: Vec<String>,
}

impl ReloadReport {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.rebuilt.is_empty()
    }

    /// One line for the log; empty when nothing changed.
    pub fn summary(&self) -> String {
        if self.is_noop() {
            return String::new();
        }
        let mut parts = Vec::new();
        for (label, ids) in [
            ("added", &self.added),
            ("rebuilt", &self.rebuilt),
            ("removed", &self.removed),
        ] {
            if !ids.is_empty() {
                parts.push(format!("{label}: {}", ids.join(", ")));
            }
        }
        parts.join("; ")
    }
}

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
        // Catalog lookup for aliases (e.g. chatgpt -> openai)
        if let Some(desc) = pacode_auth::catalog::find(id)
            && let Some(acc) = store.get(desc.id)
        {
            return ResolvedCredential::StoreOAuth(acc.clone());
        }
    }

    ResolvedCredential::None
}

/// Synthesize built-in provider entries so `/login moonshot` or `OPENROUTER_API_KEY` work
/// with an empty config.toml.
///
/// A builtin (see [`crate::builtin`]) is added when the config has no entry of that id and
/// the store holds a login for it or its API-key environment variable is set. Local
/// servers (Ollama, LM Studio) are added only once logged in. `devin` takes its base URL
/// from the account's `extra["api_server_url"]`. The OpenCode Zen free tier (`opencode`,
/// see [`synthesize_opencode`]) is always added.
pub fn synthesize_builtin_providers(
    providers: &mut BTreeMap<String, ProviderConfig>,
    store: &AuthStore,
) {
    // OpenCode Zen free tier — always available, no credentials required.
    synthesize_opencode(providers);
    for builtin in crate::builtin::builtin_providers() {
        if providers.contains_key(builtin.id) {
            continue;
        }
        if store.get(builtin.id).is_some() || builtin.env_key().is_some() {
            providers.insert(builtin.id.to_string(), builtin.to_config());
        }
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

/// Build the transport for one entry through the dialect table (see
/// [`crate::dialect`]), refusing the removed Anthropic provider first: its login is
/// gone from `pacode-auth`, so an old `kind = "anthropic"` (or `dialect = "anthropic"`)
/// entry fails with a clear error instead of a confusing auth failure later.
fn build_transport(
    id: &str,
    pcfg: &ProviderConfig,
    ctx: &DialectContext,
    credential: &ResolvedCredential,
) -> Result<Arc<dyn Provider>, ProviderError> {
    if pcfg.effective_dialect() == ProviderDialect::Anthropic {
        return Err(ProviderError::Config(format!(
            "provider '{id}': the anthropic provider kind is no longer supported"
        )));
    }
    crate::dialect::build_transport(id, pcfg, ctx, credential)
}

/// Inject the OpenCode Zen free-tier provider unconditionally.
///
/// The free tier uses `api_key = "public"` and opencode-specific headers.
/// No credentials are needed; this runs even without an AuthStore.
///
/// The session/request ids are drawn once per process, not once per call:
/// they are already documented as process-stable, and generating them here
/// would make the synthesized entry differ on every call. Hot reload compares
/// entries by value, so a fresh id each time would rebuild the provider — and
/// drop its catalog — on every config write, forever.
pub fn synthesize_opencode(providers: &mut BTreeMap<String, ProviderConfig>) {
    if providers.contains_key("opencode") {
        return;
    }
    static SESSION_ID: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(crate::opencode::random_hex_id);
    static REQUEST_ID: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(crate::opencode::random_hex_id);

    let mut headers = BTreeMap::new();
    headers.insert(
        "User-Agent".to_string(),
        format!("opencode/{}", crate::opencode::OPENCODE_VERSION),
    );
    headers.insert("x-opencode-client".to_string(), "cli".to_string());
    // Session ID is stable per pacode process; request ID is static here
    // but the server does not enforce uniqueness per-request.
    headers.insert("x-opencode-session".to_string(), SESSION_ID.clone());
    headers.insert("x-opencode-request".to_string(), REQUEST_ID.clone());
    providers.insert(
        "opencode".to_string(),
        ProviderConfig {
            kind: ProviderKind::OpenCode,
            base_url: crate::opencode::OPENCODE_ZEN_BASE_URL.to_string(),
            catalog: false, // catalog fetched via opencode::fetch_free_models, not /models
            api_key: Some("public".to_string()),
            headers,
            ..ProviderConfig::default()
        },
    );
}

/// The route a session opens with when nothing names one explicitly.
///
/// `[provider].default` wins, as it always has. When it is absent, a provider
/// that declares a `default_model` supplies both halves of the route — that is
/// the whole point of declaring it: a new provider becomes the default for its
/// own model without a second edit. Ties between such providers go to the
/// higher `priority`, then the alphabetically first id.
pub fn resolve_default_route(cfg: &Config) -> Option<ModelRoute> {
    if let Some(route) = cfg.default_route() {
        return Some(route);
    }
    let id = cfg.default_provider()?;
    let model = cfg
        .providers
        .get(id)
        .and_then(ProviderConfig::default_model)?;
    Some(ModelRoute::new(id, model))
}

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<BTreeMap<String, Arc<dyn Provider>>>>,
    default_route: Arc<RwLock<Option<ModelRoute>>>,
    catalog_cache: Arc<CatalogCache>,
    configs: Arc<RwLock<BTreeMap<String, ProviderConfig>>>,
    defaults: Arc<RwLock<ProviderDefaults>>,
    pricing: Arc<RwLock<BTreeMap<String, Pricing>>>,
    store_providers: Arc<RwLock<BTreeMap<String, String>>>,
    /// Credential file the store providers came from, so a refresh reads and writes
    /// the same file the registry was built from rather than a global default.
    auth_path: Option<std::path::PathBuf>,
    token_refresher: Option<TokenRefresher>,
    /// Resolved store credentials with their expiry; see `registry_cache` for the cap,
    /// invalidation and ownership rules. Seeded when providers are built from a store.
    /// (Per-provider concurrency gates are process-wide, see [`crate::provider_gate`].)
    token_cache: Arc<TokenCache>,
    /// Ids registered by the host rather than by config (`insert`), which a
    /// [`ProviderRegistry::reload`] must not drop: they exist in no config file,
    /// so "absent from the config" says nothing about whether they are still
    /// wanted. See [`ProviderRegistry::insert`].
    pinned: Arc<RwLock<BTreeSet<String>>>,
}

impl ProviderRegistry {
    pub fn empty() -> Self {
        Self {
            providers: Arc::new(RwLock::new(BTreeMap::new())),
            default_route: Arc::new(RwLock::new(None)),
            catalog_cache: Arc::new(CatalogCache::new(
                None,
                DEFAULT_CATALOG_TTL_SECS,
                DEFAULT_MAX_MODELS_PER_PROVIDER,
            )),
            configs: Arc::new(RwLock::new(BTreeMap::new())),
            defaults: Arc::new(RwLock::new(ProviderDefaults::default())),
            pricing: Arc::new(RwLock::new(BTreeMap::new())),
            store_providers: Arc::new(RwLock::new(BTreeMap::new())),
            auth_path: None,
            token_refresher: None,
            token_cache: Arc::new(TokenCache::default()),
            pinned: Arc::new(RwLock::new(BTreeSet::new())),
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
        // OpenCode Zen free tier is always available (no credentials needed).
        synthesize_opencode(&mut all_providers);
        if let Some(store) = store {
            registry.auth_path = Some(store.path().to_path_buf());
            synthesize_builtin_providers(&mut all_providers, store);
        }

        let mut store_providers = BTreeMap::new();
        let mut configs = BTreeMap::new();

        for (id, mut pcfg) in all_providers {
            let credential = resolve_provider_credential(&id, &pcfg, api_keys, store);

            // Devin api_server_url from extra if base_url is empty
            if pcfg.effective_dialect() == ProviderDialect::Devin
                && pcfg.base_url.trim().is_empty()
                && let ResolvedCredential::StoreOAuth(acc) = &credential
                && let Some(url) = acc.extra.get("api_server_url").and_then(|v| v.as_str())
            {
                pcfg.base_url = url.to_string();
            }

            if pcfg.base_url.trim().is_empty() && !base_url_optional(pcfg.effective_dialect()) {
                return Err(ProviderError::Config(format!(
                    "provider '{id}' has empty base_url"
                )));
            }

            if let ResolvedCredential::StoreOAuth(acc) = &credential {
                store_providers.insert(id.clone(), acc.access.clone());
                registry.token_cache.put(&id, &acc.access, acc.expires_at);
            }

            configs.insert(id.clone(), pcfg.clone());

            let ctx = DialectContext {
                defaults: cfg.provider.clone(),
                pricing: cfg.pricing.clone(),
            };
            match build_transport(&id, &pcfg, &ctx, &credential) {
                Ok(provider) => {
                    registry.insert_unpinned(provider);
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
        registry.set_default_route(resolve_default_route(cfg));
        Ok(registry)
    }

    pub fn set_token_refresher(&mut self, refresher: TokenRefresher) {
        self.token_refresher = Some(refresher);
    }

    pub fn with_catalog_cache(mut self, cache: Arc<CatalogCache>) -> Self {
        self.catalog_cache = cache;
        self
    }

    /// Apply a new config in place: rebuild the providers whose entry changed,
    /// add the new ones, drop the ones that are gone, and report what moved.
    ///
    /// The in-flight turn is not disturbed. A session already holding
    /// `Arc<dyn Provider>` for a removed provider keeps that instance until it
    /// finishes, and only *new* `get` calls see the new world — the same
    /// guarantee that lets a `Codex` token refresh swap a transport underneath
    /// a running turn. Swapping the whole registry instead would silently
    /// change the provider mid-conversation, and would also throw away the
    /// shared [`CatalogCache`], so every catalog would be refetched.
    ///
    /// A provider that cannot be built does not abort the reload: the rest of
    /// the config still applies, and the error is returned so the caller can
    /// surface it. A single bad entry must not take every other provider down
    /// with it.
    pub fn reload(
        &self,
        cfg: &Config,
        api_keys: &BTreeMap<String, Option<String>>,
        store: Option<&AuthStore>,
    ) -> Result<ReloadReport, ProviderError> {
        let mut all_providers = cfg.providers.clone();
        if let Some(store) = store {
            synthesize_builtin_providers(&mut all_providers, store);
        }
        synthesize_opencode(&mut all_providers);

        let old_configs = self
            .configs
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();

        let ctx = DialectContext {
            defaults: cfg.provider.clone(),
            pricing: cfg.pricing.clone(),
        };
        let mut report = ReloadReport::default();
        let mut next_configs = BTreeMap::new();
        let mut next_store_providers = BTreeMap::new();

        for (id, mut pcfg) in all_providers {
            let credential = resolve_provider_credential(&id, &pcfg, api_keys, store);

            if pcfg.effective_dialect() == ProviderDialect::Devin
                && pcfg.base_url.trim().is_empty()
                && let ResolvedCredential::StoreOAuth(acc) = &credential
                && let Some(url) = acc.extra.get("api_server_url").and_then(|v| v.as_str())
            {
                pcfg.base_url = url.to_string();
            }

            if let ResolvedCredential::StoreOAuth(acc) = &credential {
                next_store_providers.insert(id.clone(), acc.access.clone());
                self.token_cache.put(&id, &acc.access, acc.expires_at);
            }
            next_configs.insert(id.clone(), pcfg.clone());

            let unchanged = old_configs.get(&id) == Some(&pcfg);
            if unchanged
                && self
                    .providers
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains_key(&id)
            {
                // A config entry of this id owns the slot even when it did not
                // change: the pin that protected a host-registered shim no longer
                // applies, so a later reload may drop it if the entry goes away.
                self.unpin(&id);
                report.unchanged.push(id);
                continue;
            }

            match build_transport(&id, &pcfg, &ctx, &credential) {
                Ok(provider) => {
                    // A config entry of this id now owns the slot: the pin that
                    // protected a host-registered shim no longer applies, so a
                    // later reload may drop it again if the entry goes away.
                    self.unpin(&id);
                    self.insert_unpinned(provider);
                    // A changed entry may answer `/models` differently now, and a
                    // new one may shadow an id that was absent. Either way the
                    // cached list belongs to the old config.
                    self.catalog_cache.invalidate(&id);
                    if old_configs.contains_key(&id) {
                        report.rebuilt.push(id);
                    } else {
                        report.added.push(id);
                    }
                }
                Err(e) => {
                    if cfg.providers.contains_key(&id) {
                        return Err(e);
                    }
                    // A synthesized entry that will not build is not a config
                    // error: the account it needs is simply not logged in.
                    report.removed.push(id);
                }
            }
        }

        {
            let mut providers = self.providers.write().unwrap_or_else(|p| p.into_inner());
            let gone: Vec<String> = providers
                .keys()
                .filter(|id| !next_configs.contains_key(*id) && !self.is_pinned(id))
                .cloned()
                .collect();
            for id in gone {
                providers.remove(&id);
                self.catalog_cache.invalidate(&id);
                self.token_cache.remove(&id);
                report.removed.push(id);
            }
        }

        report.removed.sort_unstable();
        report.removed.dedup();

        *self.configs.write().unwrap_or_else(|p| p.into_inner()) = next_configs;
        *self
            .store_providers
            .write()
            .unwrap_or_else(|p| p.into_inner()) = next_store_providers;
        *self.defaults.write().unwrap_or_else(|p| p.into_inner()) = cfg.provider.clone();
        *self.pricing.write().unwrap_or_else(|p| p.into_inner()) = cfg.pricing.clone();
        self.apply_default_route(resolve_default_route(cfg));
        Ok(report)
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

    /// Register a provider the host owns rather than one the config declares —
    /// a mock in tests and smoke mode, or a process-local shim. The entry is
    /// *pinned*: a later [`ProviderRegistry::reload`] leaves it alone, because
    /// the config it is absent from is not evidence that it went away.
    ///
    /// A config entry of the same id still wins on the next reload: the reload
    /// rebuilds from config and unpins, so an explicit declaration takes over
    /// the shim it shadows.
    pub fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.insert_unpinned(provider.clone());
        self.pin(provider.id());
    }

    /// Store a provider without pinning it — the config-driven path, where the
    /// entry's presence in the config is what decides its lifetime. `insert`
    /// pins; this does not, and only the config path may use it.
    fn insert_unpinned(&self, provider: Arc<dyn Provider>) {
        self.providers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(provider.id().to_string(), provider);
    }

    fn pin(&self, id: &str) {
        self.pinned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_string());
    }

    fn unpin(&self, id: &str) {
        self.pinned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    /// Set the default route outright, including to `None`.
    pub fn set_default_route(&self, route: Option<ModelRoute>) {
        *self
            .default_route
            .write()
            .unwrap_or_else(|p| p.into_inner()) = route;
    }

    /// Apply the route a config resolves to, but only when it resolves to
    /// something. A config that names no route must not erase a route the host
    /// installed itself (`PACODE_SMOKE` mock, tests): an open session resolves
    /// through the registry first, and a config that simply says nothing about
    /// the default is not a request to drop it.
    fn apply_default_route(&self, resolved: Option<ModelRoute>) {
        if let Some(route) = resolved {
            self.set_default_route(Some(route));
        }
    }

    /// Whether `id` was registered by the host and so survives a reload.
    fn is_pinned(&self, id: &str) -> bool {
        self.pinned
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(id)
    }

    pub fn default_route(&self) -> Option<ModelRoute> {
        self.default_route
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
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
    ///
    /// When the named provider is gone from the config (a hot reload dropped it)
    /// or its credential cannot be used, the walk follows that provider's
    /// `fallbacks` and the same for each one after it, in declaration order. A
    /// provider already visited is never revisited, so a cycle in the config
    /// terminates instead of looping; the first error is what the caller sees
    /// if nothing in the chain can serve the route.
    pub async fn resolve(&self, route: &ModelRoute) -> Result<Arc<dyn Provider>, ProviderError> {
        let mut current = route.clone();
        let mut visited: Vec<String> = Vec::new();
        loop {
            if let Some(provider) = self.resolve_one(&current).await {
                return Ok(provider);
            }
            let next = self
                .fallback_for(&current.provider)
                .filter(|id| !visited.iter().any(|seen| seen == id));
            match next {
                Some(id) => {
                    visited.push(current.provider.clone());
                    log::warn!(
                        "provider '{}' cannot serve {}; falling back to '{id}'",
                        current.provider,
                        current.model
                    );
                    current = ModelRoute::new(id, current.model.clone());
                }
                None => {
                    return Err(ProviderError::Config(format!(
                        "unknown provider '{}'",
                        route.provider
                    )));
                }
            }
        }
    }

    /// The single-provider half of [`Self::resolve`], with the store-refresh
    /// step. `None` means "this provider cannot serve the route right now", not
    /// an error: a hot reload may have dropped it between turns.
    async fn resolve_one(&self, route: &ModelRoute) -> Option<Arc<dyn Provider>> {
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
                    Err(e) => {
                        log::warn!("credential refresh for '{}' failed: {e}", route.provider);
                        None
                    }
                }
            } else if legacy_resolve() {
                // Escape hatch: the pre-cache behaviour (read and rewrite per step).
                match self.refresh_from_store_legacy(&route.provider).await {
                    Ok(token) => Some(token),
                    Err(e) => {
                        log::warn!("credentials for '{}' are unusable: {e}", route.provider);
                        None
                    }
                }
            } else {
                // No seam installed: answer from the in-memory cache while the token is
                // outside the refresh window; otherwise one single-flight refresh through
                // the auth crate. A credential that needs no refresh (api key, devin)
                // comes back unchanged and is cached without an expiry.
                let provider_id = route.provider.as_str();
                match self
                    .token_cache
                    .ensure_fresh(provider_id, move || self.refresh_from_store(provider_id))
                    .await
                {
                    Ok(token) => Some(token),
                    Err(e) => {
                        log::warn!("credentials for '{}' are unusable: {e}", route.provider);
                        None
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
                    let expiry = self.token_cache.expiry(&route.provider);
                    if let Err(e) = self.rebuild_provider_with_token(&route.provider, &token) {
                        log::warn!(
                            "rebuilding provider '{}' with a fresh token failed: {e}",
                            route.provider
                        );
                        return None;
                    }
                    // The rebuild invalidated the entry; re-store the refreshed one.
                    self.token_cache.put(&route.provider, &token, expiry);
                }
            }
        }

        self.get(&route.provider)
    }

    /// The first fallback of `id` that is not already being tried.
    fn fallback_for(&self, id: &str) -> Option<String> {
        let configs = self.configs.read().unwrap_or_else(|p| p.into_inner());
        configs
            .get(id)
            .and_then(|cfg| cfg.fallbacks.first().cloned())
    }

    /// Refresh the stored credential of `provider` against the credential file this
    /// registry was built from. Returns the token and its expiry. The file is loaded in
    /// `spawn_blocking`; it is saved (also in `spawn_blocking`) only when the refresh
    /// changed the account, after re-reading it so other providers' or processes' newer
    /// entries are merged rather than overwritten.
    async fn refresh_from_store(&self, provider: &str) -> Result<(String, Option<i64>), String> {
        let Some(path) = self.auth_path.clone() else {
            let token = pacode_auth::flows::access_token(provider)
                .await
                .map_err(|e| e.to_string())?;
            // Unknown expiry: never treat as fresh.
            return Ok((token, Some(0)));
        };
        let load_path = path.clone();
        let mut store = tokio::task::spawn_blocking(move || AuthStore::load_from(load_path))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let before = store.get(provider).cloned();
        let token = pacode_auth::flows::access_token_in(&mut store, provider)
            .await
            .map_err(|e| e.to_string())?;
        let after = store.get(provider).cloned();
        let expires_at = after.as_ref().and_then(|a| a.expires_at);
        if after != before
            && let Some(account) = after
        {
            let provider = provider.to_string();
            tokio::task::spawn_blocking(move || {
                let mut latest = AuthStore::load_from(&path).unwrap_or(store);
                latest.upsert(&provider, account);
                latest.save().map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())??;
        }
        Ok((token, expires_at))
    }

    /// Pre-cache behaviour, kept behind `PACODE_AUTH_LEGACY_RESOLVE=1` for one release.
    async fn refresh_from_store_legacy(&self, provider: &str) -> Result<String, String> {
        match &self.auth_path {
            Some(path) => {
                let mut store = AuthStore::load_from(path).map_err(|e| e.to_string())?;
                let token = pacode_auth::flows::access_token_in(&mut store, provider)
                    .await
                    .map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                Ok(token)
            }
            None => pacode_auth::flows::access_token(provider)
                .await
                .map_err(|e| e.to_string()),
        }
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
        self.token_cache.remove(id);

        let ctx = DialectContext { defaults, pricing };
        let provider = build_transport(id, &pcfg, &ctx, &credential)?;
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
            self.token_cache.remove(id);
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
            if pcfg.effective_dialect() == ProviderDialect::Devin
                && pcfg.base_url.trim().is_empty()
                && let Some(url) = acc.extra.get("api_server_url").and_then(|v| v.as_str())
            {
                pcfg.base_url = url.to_string();
            }
            self.store_providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id.to_string(), acc.access.clone());
            self.token_cache.put(id, &acc.access, acc.expires_at);
        } else {
            self.store_providers
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(id);
            self.token_cache.remove(id);
            // A synthesized builtin whose login is gone (and with no env key) disappears;
            // a config-defined entry stays even if it is merely unauthenticated.
            let synthesized = id == "devin"
                || crate::builtin::find_builtin(id).is_some_and(|b| b.to_config() == pcfg);
            if synthesized
                && store.accounts(id).is_empty()
                && matches!(credential, ResolvedCredential::None)
            {
                configs.remove(id);
                self.providers
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(id);
                return Ok(());
            }
        }

        let ctx = DialectContext { defaults, pricing };
        match build_transport(id, &pcfg, &ctx, &credential) {
            Ok(provider) => {
                self.providers
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.to_string(), provider);
                configs.insert(id.to_string(), pcfg);
                Ok(())
            }
            Err(e) => {
                if pcfg.effective_dialect() == ProviderDialect::Devin {
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
    ///
    /// A bare model id that some provider names as its `default_model` lands on
    /// that provider, which is what makes a declaratively added provider usable
    /// without also editing `[provider].default`. Otherwise the default route
    /// decides, and finally the first provider in configured order.
    pub fn parse_route(&self, s: &str) -> Option<ModelRoute> {
        let ids = self.ids();
        let trimmed = s.trim();
        if !trimmed.is_empty()
            && !trimmed.contains('/')
            && let Some(provider) = self.provider_for_model(trimmed)
        {
            return Some(ModelRoute::new(provider, trimmed));
        }
        let fallback = self
            .default_route()
            .map(|r| r.provider)
            .or_else(|| self.ordered_ids().into_iter().next());
        ModelRoute::parse(trimmed, ids.iter().map(String::as_str), fallback.as_deref())
    }

    /// The highest-priority provider that names `model` as its `default_model`.
    fn provider_for_model(&self, model: &str) -> Option<String> {
        // The order is taken first and the lock dropped: `ordered_ids` takes the
        // same `configs` lock, and `std::sync::RwLock` is not reentrant, so
        // holding it across that call can park a reader behind a waiting writer.
        let ordered = self.ordered_ids();
        let configs = self.configs.read().unwrap_or_else(|p| p.into_inner());
        ordered.into_iter().find(|id| {
            configs
                .get(id)
                .and_then(ProviderConfig::default_model)
                .is_some_and(|declared| declared == model)
        })
    }

    /// Provider ids in the order a bare name should try them: highest
    /// `priority` first, alphabetically otherwise. Mirrors
    /// [`pacode_types::Config::provider_order`] for the in-memory registry, which
    /// also holds synthesized entries that are in no config file.
    ///
    /// Owned strings, not borrows: the caller must be free to drop the lock
    /// before it uses the result.
    fn ordered_ids(&self) -> Vec<String> {
        let configs = self.configs.read().unwrap_or_else(|p| p.into_inner());
        let mut ids: Vec<(i32, &String)> = configs.iter().map(|(id, c)| (c.priority, id)).collect();
        if ids.iter().any(|(priority, _)| *priority != 0) {
            ids.sort_by(|(pa, a), (pb, b)| pb.cmp(pa).then_with(|| a.cmp(b)));
        }
        ids.into_iter().map(|(_, id)| id.clone()).collect()
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
