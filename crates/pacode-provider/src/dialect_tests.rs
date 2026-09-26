use super::*;
use crate::registry::ResolvedCredential;
use pacode_types::{ProviderCompat, ProviderKind, ProviderTimeouts, WireTool};

fn ctx() -> DialectContext {
    DialectContext::default()
}

/// A provider entry, spelled the way a config would: only the fields a user
/// writes. Everything else defaults, which is what makes a new provider a
/// four-line TOML block. The id lives in the registry's key, not the entry, so
/// the helper takes the fields a test actually varies.
fn entry_config(base_url: &str, kind: ProviderKind) -> ProviderConfig {
    ProviderConfig {
        base_url: base_url.to_string(),
        kind,
        catalog: true,
        ..Default::default()
    }
}

/// Every enum variant has a factory, so no `match` elsewhere needs a fallback
/// arm and an unimplemented protocol fails here rather than at first request.
#[test]
fn every_dialect_has_a_factory() {
    let covered: Vec<&str> = known_dialects().map(|d| d.as_str()).collect();
    for expected in [
        "openai_compatible",
        "anthropic",
        "codex",
        "devin",
        "opencode_zen",
    ] {
        assert!(covered.contains(&expected), "missing dialect {expected}");
    }
    // The table must not have silent extras: a row whose dialect is not in the
    // enum would be unreachable config, and one duplicated would shadow.
    assert_eq!(covered.len(), 5, "dialect table changed: {covered:?}");
    let mut sorted = covered.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), covered.len(), "duplicate dialect row");
}

/// A dialect with no table row would panic in `entry`; walking the table keeps
/// the guarantee tested rather than assumed.
#[test]
fn every_table_row_resolves_to_itself() {
    for dialect in known_dialects() {
        assert_eq!(entry(dialect).dialect, dialect);
    }
}

/// `base_url` is mandatory for every protocol except the one that derives its
/// host from the stored credential. The registry's empty-base_url check reads
/// this flag, so a wrong answer here means a broken provider entry.
#[test]
fn base_url_optional_only_for_codex() {
    for dialect in known_dialects() {
        let expected = matches!(dialect, ProviderDialect::Codex);
        assert_eq!(
            base_url_optional(dialect),
            expected,
            "base_url_optional wrong for {}",
            dialect.as_str()
        );
    }
}

/// An explicit `dialect` overrides the legacy `kind`: that is the whole point
/// of the split, so a provider can speak Anthropic while keeping `kind` for
/// backwards-compatible consumers.
#[test]
fn explicit_dialect_wins_over_kind() {
    let mut cfg = entry_config("https://example.test", ProviderKind::Anthropic);
    assert!(!cfg.has_explicit_dialect());
    assert_eq!(cfg.effective_dialect(), ProviderDialect::Anthropic);
    cfg.dialect = Some(ProviderDialect::OpenAiCompatible);
    assert!(cfg.has_explicit_dialect());
    assert_eq!(cfg.effective_dialect(), ProviderDialect::OpenAiCompatible);
    // The legacy field is untouched, so old readers still see `kind`.
    assert_eq!(cfg.kind, ProviderKind::Anthropic);
}

/// With no explicit dialect the legacy `kind` still selects the protocol, so
/// pre-existing configs behave identically.
#[test]
fn legacy_kind_selects_dialect() {
    for (kind, dialect) in [
        (ProviderKind::OpenAi, ProviderDialect::OpenAiCompatible),
        (ProviderKind::Anthropic, ProviderDialect::Anthropic),
        (ProviderKind::Codex, ProviderDialect::Codex),
        (ProviderKind::Devin, ProviderDialect::Devin),
        (ProviderKind::OpenCode, ProviderDialect::OpenCodeZen),
    ] {
        let cfg = entry_config("https://example.test", kind);
        assert_eq!(cfg.effective_dialect(), dialect, "kind {kind:?}");
        assert_eq!(cfg.effective_dialect(), ProviderDialect::from_kind(kind));
    }
}

/// The generic path: any URL, no credentials, builds an OpenAI-compatible
/// transport. This is the property that makes new vendors a TOML entry.
#[test]
fn arbitrary_openai_compatible_endpoint_needs_no_code() {
    let cfg = ProviderConfig {
        dialect: Some(ProviderDialect::OpenAiCompatible),
        ..entry_config("https://myproxy.internal/v1", ProviderKind::OpenAi)
    };
    let provider = build_transport("myproxy", &cfg, &ctx(), &ResolvedCredential::None)
        .expect("an OpenAI-compatible endpoint must build with no credential");
    assert_eq!(provider.id(), "myproxy");
}

#[test]
fn openai_compatible_builds_with_api_key() {
    let cfg = entry_config("https://api.groq.com/openai/v1", ProviderKind::OpenAi);
    let provider = build_transport(
        "groq",
        &cfg,
        &ctx(),
        &ResolvedCredential::ConfigApiKey("gsk_test".into()),
    )
    .expect("build");
    assert_eq!(provider.id(), "groq");
}

/// A per-provider timeout axis only changes something when the entry sets it:
/// absent fields must fall back to the global `[provider]` defaults.
#[test]
fn per_provider_timeouts_fall_back_when_unset() {
    let unset = ProviderTimeouts::default();
    assert_eq!(unset.connect_secs_or(30), 30);
    assert_eq!(unset.stream_idle_secs_or(120), 120);
    assert_eq!(unset.max_retries_or(5), 5);

    let cfg = entry_config("https://e.test", ProviderKind::OpenAi);
    assert_eq!(
        cfg.timeouts.connect_secs_or(30),
        30,
        "a config without a timeouts block must not override the default"
    );

    let tuned = ProviderTimeouts {
        connect_secs: Some(7),
        stream_idle_secs: Some(11),
        max_retries: Some(1),
    };
    let cfg = ProviderConfig {
        timeouts: tuned.clone(),
        ..entry_config("https://e.test", ProviderKind::OpenAi)
    };
    assert_eq!(cfg.timeouts.connect_secs_or(30), 7);
    assert_eq!(cfg.timeouts.stream_idle_secs_or(120), 11);
    assert_eq!(cfg.timeouts.max_retries_or(5), 1);
}

/// The compatibility axes default to what pacode did before they existed, so
/// an entry that sets none behaves exactly like a pre-compat config.
#[test]
fn compat_defaults_match_legacy_behaviour() {
    let compat = ProviderCompat::default();
    assert_eq!(compat, ProviderConfig::default().compat);
    assert!(compat.sends_max_output_tokens());
    assert!(!compat.thinking_mode.sends_for_reasoning_model(false));
    assert!(compat.thinking_mode.sends_for_reasoning_model(true));
    assert_eq!(compat.wire_tools(&[]), Vec::<WireTool>::new());
}
