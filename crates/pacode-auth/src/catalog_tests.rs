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

    assert_eq!(LOGIN_PROVIDERS.len(), 14);

    let mut orders: Vec<u8> = LOGIN_PROVIDERS.iter().map(|p| p.order).collect();
    let sorted = {
        let mut o = orders.clone();
        o.sort_unstable();
        o.dedup();
        o
    };
    orders.dedup();
    assert_eq!(orders, sorted, "orders must be unique and listed ascending");
}

#[test]
fn test_find_by_id() {
    let ids = [
        "openai",
        "openai-api",
        "openrouter",
        "moonshot",
        "kimi-code",
        "zai",
        "zai-coding",
        "opencode-zen",
        "opencode-go",
        "gemini",
        "devin",
        "ollama",
        "lmstudio",
        "custom",
    ];

    for id in ids {
        let provider = find(id);
        assert!(provider.is_some(), "should find provider by id: {id}");
        assert_eq!(provider.unwrap().id, id);
    }
}

#[test]
fn test_anthropic_is_not_supported() {
    for name in ["claude", "anthropic", "anthropic-api", "claude-api"] {
        assert!(find(name).is_none(), "{name} must not resolve");
    }
    for p in LOGIN_PROVIDERS {
        assert!(!p.id.contains("claude") && !p.id.contains("anthropic"));
        assert!(!p.display_name.to_ascii_lowercase().contains("claude"));
        assert!(!p.display_name.to_ascii_lowercase().contains("anthropic"));
    }
}

#[test]
fn test_find_by_alias() {
    let openai = find("chatgpt").expect("find openai via chatgpt");
    assert_eq!(openai.id, "openai");
    assert_eq!(openai.auth_kind, AuthKind::OAuth);

    let kimi = find("kimi").expect("find moonshot via kimi");
    assert_eq!(kimi.id, "moonshot");
    assert_eq!(kimi.auth_kind, AuthKind::ApiKey);

    let gemini = find("google-ai-studio").expect("find gemini");
    assert_eq!(gemini.id, "gemini");

    let devin = find("cognition").expect("find devin via cognition");
    assert_eq!(devin.id, "devin");

    let custom = find("compat").expect("find custom via compat");
    assert_eq!(custom.id, "custom");
}

#[test]
fn test_find_case_insensitive_and_whitespace() {
    assert_eq!(find("  OPENAI  ").map(|p| p.id), Some("openai"));
    assert_eq!(find("ChatGPT").map(|p| p.id), Some("openai"));
    assert_eq!(find("  OpenRouter  ").map(|p| p.id), Some("openrouter"));
    assert_eq!(find("Z.AI Coding Plan").map(|p| p.id), Some("zai-coding"));
}

#[test]
fn test_find_unknown_returns_none() {
    assert!(find("unknown-provider-xyz").is_none());
    assert!(find("").is_none());
    assert!(find("   ").is_none());
}
