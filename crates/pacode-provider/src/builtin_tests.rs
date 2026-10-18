use std::collections::HashSet;

use pacode_auth::catalog::find;
use pacode_types::ProviderKind;

use super::*;

#[test]
fn ids_are_unique_and_urls_are_well_formed() {
    let mut seen = HashSet::new();
    for b in builtin_providers() {
        assert!(seen.insert(b.id), "duplicate builtin id {}", b.id);
        assert!(
            b.base_url.starts_with("https://") || b.base_url.starts_with("http://localhost"),
            "{}: bad base url {}",
            b.id,
            b.base_url
        );
        assert!(!b.base_url.ends_with('/'), "{}: trailing slash", b.id);
    }
}

#[test]
fn every_builtin_has_a_matching_login_entry() {
    for b in builtin_providers() {
        let login = find(b.id).unwrap_or_else(|| panic!("no login entry for {}", b.id));
        assert_eq!(login.id, b.id);
        assert_eq!(login.auth_kind, b.auth, "{}: auth kind drift", b.id);
    }
}

#[test]
fn required_providers_and_urls() {
    let url = |id: &str| find_builtin(id).map(|b| b.base_url).unwrap_or("");
    assert_eq!(url("openrouter"), "https://openrouter.ai/api/v1");
    assert_eq!(url("moonshot"), "https://api.moonshot.ai/v1");
    assert_eq!(url("kimi-code"), "https://api.kimi.com/coding/v1");
    assert_eq!(url("zai"), "https://api.z.ai/api/paas/v4");
    assert_eq!(url("zai-coding"), "https://api.z.ai/api/coding/paas/v4");
    assert_eq!(url("opencode-zen"), "https://opencode.ai/zen/v1");
    assert_eq!(url("opencode-go"), "https://opencode.ai/zen/go/v1");
    assert_eq!(url("openai-api"), "https://api.openai.com/v1");
    assert_eq!(
        url("gemini"),
        "https://generativelanguage.googleapis.com/v1beta/openai"
    );
    assert_eq!(
        find_builtin("openai").map(|b| b.kind),
        Some(ProviderKind::Codex)
    );
}

#[test]
fn env_vars_match_the_documented_names() {
    let env = |id: &str| find_builtin(id).and_then(|b| b.api_key_env);
    assert_eq!(env("openrouter"), Some("OPENROUTER_API_KEY"));
    assert_eq!(env("moonshot"), Some("MOONSHOT_API_KEY"));
    assert_eq!(env("kimi-code"), Some("KIMI_API_KEY"));
    assert_eq!(env("zai"), Some("ZAI_API_KEY"));
    assert_eq!(env("zai-coding"), Some("ZAI_API_KEY"));
    assert_eq!(env("opencode-zen"), Some("OPENCODE_API_KEY"));
    assert_eq!(env("opencode-go"), Some("OPENCODE_API_KEY"));
    assert_eq!(env("openai-api"), Some("OPENAI_API_KEY"));
    assert_eq!(env("gemini"), Some("GEMINI_API_KEY"));
}

#[test]
fn to_config_carries_models_and_env() {
    let cfg = find_builtin("zai-coding").expect("zai-coding").to_config();
    assert_eq!(cfg.api_key_env.as_deref(), Some("ZAI_API_KEY"));
    assert!(cfg.catalog);
    assert!(cfg.models.iter().any(|m| m.id == "glm-5.3-flash"));
}

#[test]
fn no_anthropic_provider_is_builtin() {
    for b in builtin_providers() {
        assert!(!b.id.contains("anthropic") && !b.id.contains("claude"));
        assert!(!b.base_url.contains("anthropic"));
    }
}
