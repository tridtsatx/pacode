use super::*;
use std::collections::HashSet;

#[test]
fn test_ids_and_aliases_unique() {
    let mut seen_ids = HashSet::new();
    let mut seen_aliases = HashSet::new();

    for p in LOGIN_PROVIDERS {
        assert!(seen_ids.insert(p.id), "duplicate provider id: {}", p.id);
        for &alias in p.aliases {
            assert!(
                seen_aliases.insert(alias),
                "duplicate provider alias: {alias} in {}",
                p.id
            );
        }
    }

    // Exactly the 9 providers required by the specification
    assert_eq!(LOGIN_PROVIDERS.len(), 9);
}

#[test]
fn test_find_by_id() {
    let ids = [
        "claude",
        "openai",
        "devin",
        "anthropic-api",
        "openai-api",
        "openrouter",
        "gemini-api",
        "ollama",
        "custom",
    ];

    for id in ids {
        let provider = find(id);
        assert!(provider.is_some(), "should find provider by id: {id}");
        assert_eq!(provider.unwrap().id, id);
    }
}

#[test]
fn test_find_by_alias() {
    let claude = find("anthropic").expect("find claude via anthropic");
    assert_eq!(claude.id, "claude");
    assert_eq!(claude.auth_kind, AuthKind::OAuth);

    let openai = find("chatgpt").expect("find openai via chatgpt");
    assert_eq!(openai.id, "openai");
    assert_eq!(openai.auth_kind, AuthKind::OAuth);

    let devin = find("cognition").expect("find devin via cognition");
    assert_eq!(devin.id, "devin");

    let anthropic_api = find("claude-api").expect("find anthropic-api");
    assert_eq!(anthropic_api.id, "anthropic-api");

    let custom = find("compat").expect("find custom via compat");
    assert_eq!(custom.id, "custom");
}

#[test]
fn test_find_case_insensitive_and_whitespace() {
    assert_eq!(find("  CLAUDE  ").map(|p| p.id), Some("claude"));
    assert_eq!(find("ChatGPT").map(|p| p.id), Some("openai"));
    assert_eq!(find("  OpenRouter  ").map(|p| p.id), Some("openrouter"));
}

#[test]
fn test_find_unknown_returns_none() {
    assert!(find("unknown-provider-xyz").is_none());
    assert!(find("").is_none());
    assert!(find("   ").is_none());
}
