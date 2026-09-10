//! Static login provider descriptor table.

/// The authentication mechanism required by a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKind {
    /// OAuth 2.0 authorization code flow with PKCE.
    OAuth,
    /// Static API key credential.
    ApiKey,
    /// Unauthenticated or local endpoint (e.g. Ollama).
    Local,
}

impl AuthKind {
    /// Human-readable label for the authentication kind.
    pub fn label(self) -> &'static str {
        match self {
            Self::OAuth => "OAuth",
            Self::ApiKey => "API key",
            Self::Local => "Local endpoint",
        }
    }
}

/// Static descriptor of a supported login provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginProvider {
    /// Canonical provider identifier (e.g. `"claude"`).
    pub id: &'static str,
    /// Human-friendly display label (e.g. `"Claude"`).
    pub display_name: &'static str,
    /// Required authentication kind.
    pub auth_kind: AuthKind,
    /// Short summary of access requirements or details.
    pub detail: &'static str,
    /// Whether this provider is recommended in login menus.
    pub recommended: bool,
    /// Menu presentation sort order (lower is shown first).
    pub order: u8,
    /// Alternate identifiers or aliases (e.g. `["anthropic"]`).
    pub aliases: &'static [&'static str],
}

/// Static registry of supported login providers.
pub const LOGIN_PROVIDERS: &[LoginProvider] = &[
    LoginProvider {
        id: "claude",
        display_name: "Claude",
        auth_kind: AuthKind::OAuth,
        detail: "Claude Pro/Max subscription",
        recommended: true,
        order: 1,
        aliases: &["anthropic"],
    },
    LoginProvider {
        id: "openai",
        display_name: "OpenAI",
        auth_kind: AuthKind::OAuth,
        detail: "ChatGPT Plus/Pro subscription",
        recommended: true,
        order: 2,
        aliases: &["chatgpt"],
    },
    LoginProvider {
        id: "devin",
        display_name: "Devin",
        auth_kind: AuthKind::OAuth,
        detail: "Devin / Cognition account",
        recommended: false,
        order: 3,
        aliases: &["cognition"],
    },
    LoginProvider {
        id: "anthropic-api",
        display_name: "Anthropic API",
        auth_kind: AuthKind::ApiKey,
        detail: "Direct Anthropic Messages API key",
        recommended: false,
        order: 4,
        aliases: &["claude-api", "anthropic-key"],
    },
    LoginProvider {
        id: "openai-api",
        display_name: "OpenAI API",
        auth_kind: AuthKind::ApiKey,
        detail: "Native OpenAI API key, pay-per-token",
        recommended: false,
        order: 5,
        aliases: &["openai-key", "openai-platform"],
    },
    LoginProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        auth_kind: AuthKind::ApiKey,
        detail: "OpenRouter API key, pay-per-token",
        recommended: false,
        order: 6,
        aliases: &["open-router"],
    },
    LoginProvider {
        id: "gemini-api",
        display_name: "Gemini API",
        auth_kind: AuthKind::ApiKey,
        detail: "Google Gemini API key",
        recommended: false,
        order: 7,
        aliases: &["gemini", "google-ai-studio"],
    },
    LoginProvider {
        id: "ollama",
        display_name: "Ollama",
        auth_kind: AuthKind::Local,
        detail: "Local Ollama instance",
        recommended: false,
        order: 8,
        aliases: &[],
    },
    LoginProvider {
        id: "custom",
        display_name: "Custom",
        auth_kind: AuthKind::ApiKey,
        detail: "Any OpenAI-compatible base_url",
        recommended: false,
        order: 9,
        aliases: &["openai-compatible", "compat"],
    },
];

/// Find a provider descriptor by canonical ID or known alias.
///
/// Input is trimmed and matched case-insensitively against `id` and `aliases`.
pub fn find(id_or_alias: &str) -> Option<&'static LoginProvider> {
    let normalized = id_or_alias.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    LOGIN_PROVIDERS.iter().find(|p| {
        p.id == normalized
            || p.aliases.iter().any(|&alias| alias == normalized)
            || p.display_name.to_ascii_lowercase() == normalized
    })
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
