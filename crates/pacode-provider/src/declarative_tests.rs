//! Tests for the declarative-provider surface: dialect selection, endpoint
//! templating, compatibility axes, and registry hot reload.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_types::{
    Config, ImageTransport, ModelConfig, ModelInfo, ModelRoute, ProviderCompat, ProviderConfig,
    ProviderDialect, ProviderKind, ProviderTimeouts, ToolResultMode, WebSearchToolType,
};

use crate::catalog_cache::CatalogCache;
use crate::registry::ProviderRegistry;

fn model_info(provider: &str, model: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider.to_string(), model.to_string()),
        display_name: model.to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }
}

fn entry(id_base: &str, kind: ProviderKind) -> ProviderConfig {
    ProviderConfig {
        base_url: format!("https://{id_base}"),
        api_key: Some("k".to_string()),
        catalog: true,
        kind,
        ..Default::default()
    }
}

fn config_with(providers: BTreeMap<String, ProviderConfig>) -> Config {
    Config {
        providers,
        ..Config::default()
    }
}

/// A registry built hermetically: no credential store and no on-disk catalog.
/// `from_config` would consult the developer's real
/// `~/.config/pacode/providers.toml` and build a cache bound to the real
/// catalog file, so a test's expectations would depend on which accounts the
/// machine happens to be logged into — a synthesized `devin` showing up in a
/// report is the symptom. Every entry these tests care about is declared in the
/// config they build.
fn registry(cfg: &Config) -> ProviderRegistry {
    ProviderRegistry::from_config_with_store(cfg, &BTreeMap::new(), None)
        .expect("registry builds")
        .with_catalog_cache(Arc::new(CatalogCache::new(None, 3600, 8)))
}

// ---------------------------------------------------------------------------
// Endpoint declaration
// ---------------------------------------------------------------------------

/// A gateway that routes by model is a config, not a fork of the adapter.
#[test]
fn path_template_substitutes_and_escapes_the_model() {
    let mut cfg = entry("gw.test", ProviderKind::OpenAi);
    cfg.path = Some("/v1/models/{model}/generate".to_string());
    assert_eq!(
        cfg.completion_url(ProviderDialect::OpenAiCompatible, "gpt-4o"),
        "https://gw.test/v1/models/gpt-4o/generate"
    );
    // A model id may contain a slash (OpenRouter) and spaces; both would change
    // the meaning of the path if they went out raw.
    assert_eq!(
        cfg.completion_url(ProviderDialect::OpenAiCompatible, "meta-llama/Llama 3"),
        "https://gw.test/v1/models/meta-llama%2FLlama%203/generate"
    );
}

/// No `path` means the dialect's own default, which is what every config
/// written before `path` existed gets.
#[test]
fn absent_path_uses_the_dialect_default() {
    let cfg = entry("x.test", ProviderKind::OpenAi);
    assert_eq!(
        cfg.completion_url(ProviderDialect::OpenAiCompatible, "m"),
        "https://x.test/chat/completions"
    );
    let mut cfg = entry("y.test", ProviderKind::Anthropic);
    cfg.path = Some("   ".to_string());
    assert_eq!(
        cfg.completion_url(ProviderDialect::Anthropic, "m"),
        "https://y.test/messages",
        "a blank path is absent, not a path of spaces"
    );
}

/// A trailing slash on `base_url` must not double up against the path.
#[test]
fn base_url_trailing_slash_is_normalized() {
    let mut cfg = entry("z.test/", ProviderKind::OpenAi);
    cfg.base_url = "https://z.test/".to_string();
    assert_eq!(
        cfg.completion_url(ProviderDialect::OpenAiCompatible, "m"),
        "https://z.test/chat/completions"
    );
    assert_eq!(cfg.models_url(), "https://z.test/models");
}

// ---------------------------------------------------------------------------
// Declarative serde: a vendor is a TOML block, not a code change
// ---------------------------------------------------------------------------

/// The whole point of the feature: a host nobody has heard of, spelled in TOML,
/// loads into a working provider entry.
#[test]
fn unknown_vendor_loads_from_toml_alone() {
    let toml = r#"
dialect = "openai_compatible"
base_url = "https://api.mysteryvendor.dev/v1"
api_key_env = "MYSTERY_KEY"
catalog = true
path = "/chat/completions"

[timeouts]
connect_secs = 7
stream_idle_secs = 90
max_retries = 2

[compat]
thinking_mode = "always"
omit_max_output_tokens = true
image_transport = "text_only"
tool_result_mode = "text_with_json"
web_search_tool_type = "provider"
apply_patch_tool_type = "none"
"#;
    let pcfg: ProviderConfig = toml::from_str(toml).expect("entry parses");
    assert_eq!(pcfg.effective_dialect(), ProviderDialect::OpenAiCompatible);
    assert!(pcfg.has_explicit_dialect());
    assert_eq!(pcfg.timeouts.connect_secs_or(30), 7);
    assert_eq!(pcfg.timeouts.stream_idle_secs_or(120), 90);
    assert_eq!(pcfg.timeouts.max_retries_or(5), 2);
    assert!(!pcfg.compat.sends_max_output_tokens());
    assert_eq!(pcfg.compat.image_transport, ImageTransport::TextOnly);
    assert_eq!(pcfg.compat.tool_result_mode, ToolResultMode::TextWithJson);
    assert_eq!(
        pcfg.compat.web_search_tool_type,
        WebSearchToolType::Provider
    );
    assert!(pcfg.compat.sends_thinking_for(false));
}

/// Every legacy `kind` value must keep deserializing, since that is the
/// backward-compatibility contract for configs written before `dialect`.
#[test]
fn legacy_kinds_still_deserialize() {
    for (toml, kind, dialect) in [
        (
            "kind = \"openai\"",
            ProviderKind::OpenAi,
            ProviderDialect::OpenAiCompatible,
        ),
        (
            "kind = \"anthropic\"",
            ProviderKind::Anthropic,
            ProviderDialect::Anthropic,
        ),
        (
            "kind = \"codex\"",
            ProviderKind::Codex,
            ProviderDialect::Codex,
        ),
        (
            "kind = \"devin\"",
            ProviderKind::Devin,
            ProviderDialect::Devin,
        ),
        (
            "kind = \"opencode\"",
            ProviderKind::OpenCode,
            ProviderDialect::OpenCodeZen,
        ),
    ] {
        let src = format!("{toml}\nbase_url = \"https://e.test\"\n");
        let pcfg: ProviderConfig = toml::from_str(&src).expect(toml);
        assert_eq!(pcfg.kind, kind, "{toml}");
        assert_eq!(pcfg.effective_dialect(), dialect, "{toml}");
        assert!(!pcfg.has_explicit_dialect());
    }
}

/// A typo in a closed axis is a load error, not a silently ignored key.
#[test]
fn unknown_compat_axis_is_rejected() {
    let toml = "base_url = \"https://e.test\"\n[compat]\nnonsense_axis = true\n";
    let err = toml::from_str::<ProviderConfig>(toml).expect_err("must not parse");
    assert!(
        err.to_string().contains("nonsense_axis"),
        "error should name the offending key, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Compatibility axes: defaults must equal the pre-axis behaviour
// ---------------------------------------------------------------------------

/// An entry that sets no axis must behave exactly like a config written before
/// the axes existed. This is the test that keeps "unset" meaning "unchanged".
#[test]
fn unset_axes_match_legacy_behaviour() {
    let compat = ProviderCompat::default();
    assert!(
        compat.sends_max_output_tokens(),
        "max_tokens still goes out"
    );
    assert_eq!(compat.image_transport, ImageTransport::DataUrl);
    assert_eq!(compat.tool_result_mode, ToolResultMode::Text);
    assert_eq!(compat.web_search_tool_type, WebSearchToolType::Native);
    assert!(!compat.sends_thinking_for(false));
    assert!(compat.sends_thinking_for(true));
}

/// Unset timeouts fall through to whatever the caller passes as the fallback,
/// so a config without a `[timeouts]` block cannot accidentally tighten a
/// bound the global defaults chose.
#[test]
fn unset_timeouts_defer_to_the_fallback() {
    let t = ProviderTimeouts::default();
    assert_eq!(t.connect_secs_or(30), 30);
    assert_eq!(t.stream_idle_secs_or(120), 120);
    assert_eq!(t.max_retries_or(5), 5);
}

// ---------------------------------------------------------------------------
// Model-level axes
// ---------------------------------------------------------------------------

#[test]
fn model_vision_and_tools_axes_override_guesses() {
    let m = ModelConfig {
        id: "m".to_string(),
        vision: Some(false),
        supports_tools: Some(false),
        ..Default::default()
    };
    assert!(!m.vision_or(true), "explicit false beats the guess");
    assert!(!m.tools_or(true));
    // An entry that says nothing defers.
    let bare = ModelConfig {
        id: "m".to_string(),
        ..Default::default()
    };
    assert!(bare.vision_or(true));
    assert!(bare.tools_or(true));
    assert!(
        !bare.tools_or(false),
        "an explicit provider default still applies"
    );
}

// ---------------------------------------------------------------------------
// Hot reload
// ---------------------------------------------------------------------------

fn two_providers() -> BTreeMap<String, ProviderConfig> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "alpha".to_string(),
        entry("alpha.test", ProviderKind::OpenAi),
    );
    providers.insert("beta".to_string(), entry("beta.test", ProviderKind::OpenAi));
    providers
}

/// A provider whose entry did not change keeps its transport, so an unrelated
/// edit elsewhere in the file does not invalidate every open session.
#[test]
fn reload_keeps_untouched_providers() {
    let cfg = config_with(two_providers());
    let reg = registry(&cfg);
    let before = reg.get("alpha").expect("alpha present");

    let report = reg.reload(&cfg, &BTreeMap::new(), None).expect("reload");
    assert!(report.unchanged.contains(&"alpha".to_string()));
    assert!(
        report.rebuilt.is_empty(),
        "nothing changed: {:?}",
        report.rebuilt
    );
    assert!(report.added.is_empty(), "nothing new: {:?}", report.added);

    let after = reg.get("alpha").expect("alpha still present");
    assert!(
        Arc::ptr_eq(&before, &after),
        "an unchanged entry must not be rebuilt"
    );
}

/// A changed `base_url` must actually produce a new transport, not leave the old
/// one serving the new config.
#[test]
fn reload_rebuilds_changed_provider() {
    let reg = registry(&config_with(two_providers()));
    let before = reg.get("alpha").expect("alpha present");

    let mut changed = two_providers();
    changed.insert(
        "alpha".to_string(),
        entry("alpha2.test", ProviderKind::OpenAi),
    );
    reg.reload(&config_with(changed), &BTreeMap::new(), None)
        .expect("reload");

    let after = reg.get("alpha").expect("alpha still present");
    assert!(
        !Arc::ptr_eq(&before, &after),
        "changed entry must be rebuilt"
    );
    assert_eq!(
        report_ids(&reg),
        vec!["alpha".to_string(), "beta".to_string()]
    );
}

fn report_ids(reg: &ProviderRegistry) -> Vec<String> {
    let mut ids = reg.ids();
    ids.retain(|id| id != "opencode");
    ids.sort();
    ids
}

/// A provider dropped from the config disappears from the registry, but a
/// session already holding its `Arc` keeps working — the old instance stays
/// valid until whoever holds it lets it go.
#[test]
fn reload_removes_gone_provider_without_breaking_issued_handles() {
    let reg = registry(&config_with(two_providers()));
    let held = reg.get("beta").expect("beta present");

    let mut only_alpha = BTreeMap::new();
    only_alpha.insert(
        "alpha".to_string(),
        entry("alpha.test", ProviderKind::OpenAi),
    );
    let report = reg
        .reload(&config_with(only_alpha), &BTreeMap::new(), None)
        .expect("reload");

    assert!(report.removed.contains(&"beta".to_string()));
    assert!(reg.get("beta").is_none(), "beta is gone for new callers");
    assert_eq!(held.id(), "beta", "the in-flight handle still works");
}

/// A provider added by a reload becomes usable, and the ones already there are
/// reported as unchanged rather than rebuilt.
#[test]
fn reload_adds_new_provider() {
    let reg = registry(&config_with(two_providers()));
    let before = reg.get("alpha").expect("alpha present");

    let mut three = two_providers();
    three.insert(
        "gamma".to_string(),
        entry("gamma.test", ProviderKind::OpenAi),
    );
    let report = reg
        .reload(&config_with(three), &BTreeMap::new(), None)
        .expect("reload");

    assert!(report.added.contains(&"gamma".to_string()));
    assert!(report.unchanged.contains(&"alpha".to_string()));
    assert!(reg.get("gamma").is_some());
    assert!(Arc::ptr_eq(&before, &reg.get("alpha").expect("alpha")));
}

/// A provider whose config is broken must not take the healthy ones down with
/// it: the reload reports the error, and what already worked still works.
#[test]
fn reload_reports_a_bad_entry_without_dropping_healthy_ones() {
    let reg = registry(&config_with(two_providers()));

    let mut broken = two_providers();
    broken.insert(
        "gamma".to_string(),
        ProviderConfig {
            base_url: String::new(),
            api_key: Some("k".to_string()),
            ..Default::default()
        },
    );
    assert!(
        reg.reload(&config_with(broken), &BTreeMap::new(), None)
            .is_err()
    );
    assert!(
        reg.get("alpha").is_some(),
        "an unbuildable new entry must not evict a working provider"
    );
}

// ---------------------------------------------------------------------------
// Catalog invalidation
// ---------------------------------------------------------------------------

/// A provider whose endpoint changed may now answer `/models` differently, so
/// its cached list must go — otherwise the picker keeps offering models the new
/// endpoint does not have.
#[test]
fn reload_invalidates_catalog_of_changed_provider_only() {
    let cfg = config_with(two_providers());
    let reg = registry(&cfg);

    let cache = reg.catalog_cache();
    cache.seed_memory_cache("alpha", 1, vec![model_info("alpha", "old")]);
    cache.seed_memory_cache("beta", 1, vec![model_info("beta", "keep")]);

    let mut changed = two_providers();
    changed.insert(
        "alpha".to_string(),
        entry("alpha2.test", ProviderKind::OpenAi),
    );
    reg.reload(&config_with(changed), &BTreeMap::new(), None)
        .expect("reload");

    assert!(
        cache.get_cached_models("alpha").is_none(),
        "the rebuilt provider's cached catalog is stale"
    );
    assert!(
        cache.get_cached_models("beta").is_some(),
        "an untouched provider keeps its catalog"
    );
}

/// Dropping a provider must drop its cached catalog too, or a provider id
/// re-added later would briefly serve a list from its previous life.
#[test]
fn reload_invalidates_catalog_of_removed_provider() {
    let reg = registry(&config_with(two_providers()));
    let cache = reg.catalog_cache();
    cache.seed_memory_cache("beta", 1, vec![model_info("beta", "gone")]);

    let mut only_alpha = BTreeMap::new();
    only_alpha.insert(
        "alpha".to_string(),
        entry("alpha.test", ProviderKind::OpenAi),
    );
    reg.reload(&config_with(only_alpha), &BTreeMap::new(), None)
        .expect("reload");

    assert!(cache.get_cached_models("beta").is_none());
}

/// `invalidate` is the primitive both reload paths lean on: memory and disk
/// both forget the provider.
#[tokio::test]
async fn catalog_invalidate_clears_memory_and_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("catalog.json");
    let cache = Arc::new(CatalogCache::new(Some(path.clone()), 3600, 8));
    let provider: Arc<dyn crate::Provider> = Arc::new(crate::mock::MockProvider::new("p"));

    // A record written by a real fetch is what a stale disk read would return.
    let models = cache.get_models_for_provider("p", &provider).await;
    assert!(!models.is_empty(), "the mock serves a catalog to fetch");
    assert!(cache.get_cached_models("p").is_some());

    cache.invalidate("p");
    assert!(cache.get_cached_models("p").is_none());
    let disk = crate::catalog_cache::load_disk_cache(&path).expect("cache file");
    assert!(
        !disk.providers.contains_key("p"),
        "the on-disk record must go too, or a restart resurrects it"
    );
}

// ---------------------------------------------------------------------------
// Host-registered providers
// ---------------------------------------------------------------------------

/// A provider the host registered (the smoke-mode mock, tests) is in no config
/// file. A reload that treated "absent from the config" as "gone" would drop it
/// and leave every session that resolves through it without a route.
#[test]
fn reload_keeps_a_host_registered_provider() {
    let mut reg = registry(&config_with(two_providers()));
    reg.insert(Arc::new(crate::mock::MockProvider::new("mock")));
    reg.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let report = reg
        .reload(&config_with(two_providers()), &BTreeMap::new(), None)
        .expect("reload");

    assert!(
        reg.get("mock").is_some(),
        "the host's provider must survive"
    );
    assert!(
        !report.removed.contains(&"mock".to_string()),
        "a host provider is not removed by a reload"
    );
    assert_eq!(
        reg.default_route(),
        Some(ModelRoute::new("mock", "mock-model")),
        "a config that names no route must not erase the installed one"
    );
}

/// The pin protects a host shim, not a config entry: once the config declares
/// that id, the entry owns the slot and can be dropped again like any other.
#[test]
fn a_config_entry_takes_over_a_pinned_id() {
    let mut reg = registry(&config_with(two_providers()));
    reg.insert(Arc::new(crate::mock::MockProvider::new("alpha")));

    reg.reload(&config_with(two_providers()), &BTreeMap::new(), None)
        .expect("reload");
    assert!(reg.get("alpha").is_some(), "the config rebuilt alpha");

    let mut only_beta = BTreeMap::new();
    only_beta.insert("beta".to_string(), entry("beta.test", ProviderKind::OpenAi));
    reg.reload(&config_with(only_beta), &BTreeMap::new(), None)
        .expect("reload");

    assert!(
        reg.get("alpha").is_none(),
        "once alpha came from config, dropping it from config drops it"
    );
}

/// A route the config does resolve to replaces the installed one, so a real
/// `[provider].default` still wins over a host-installed route.
#[test]
fn a_resolved_config_route_replaces_the_installed_one() {
    let mut reg = registry(&config_with(two_providers()));
    reg.insert(Arc::new(crate::mock::MockProvider::new("mock")));
    reg.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let mut cfg = config_with(two_providers());
    cfg.provider.default = Some("alpha/some-model".to_string());
    reg.reload(&cfg, &BTreeMap::new(), None).expect("reload");

    assert_eq!(
        reg.default_route(),
        Some(ModelRoute::new("alpha", "some-model"))
    );
}
