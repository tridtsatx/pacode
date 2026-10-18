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
    /// Canonical provider identifier (e.g. `"openai"`).
    pub id: &'static str,
    /// Human-friendly display label (e.g. `"OpenAI"`).
    pub display_name: &'static str,
    /// Required authentication kind.
    pub auth_kind: AuthKind,
    /// Short summary of access requirements or details.
    pub detail: &'static str,
    /// Whether this provider is recommended in login menus.
    pub recommended: bool,
    /// Menu presentation sort order (lower is shown first).
    pub order: u8,
    /// Alternate identifiers or aliases (e.g. `["chatgpt"]`).
    pub aliases: &'static [&'static str],
}

/// Static registry of supported login providers.
pub const LOGIN_PROVIDERS: &[LoginProvider] = &[
    LoginProvider {
        id: "openai",
        display_name: "OpenAI",
        auth_kind: AuthKind::OAuth,
        detail: "ChatGPT Plus/Pro subscription (Codex login)",
        recommended: true,
        order: 1,
        aliases: &["chatgpt", "codex"],
    },
    LoginProvider {
        id: "openai-api",
        display_name: "OpenAI API",
        auth_kind: AuthKind::ApiKey,
        detail: "OPENAI_API_KEY, pay-per-token",
        recommended: false,
        order: 2,
        aliases: &["openai-key", "openai-platform"],
    },
    LoginProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        auth_kind: AuthKind::ApiKey,
        detail: "OPENROUTER_API_KEY, hundreds of models",
        recommended: true,
        order: 3,
        aliases: &["open-router"],
    },
    LoginProvider {
        id: "moonshot",
        display_name: "Kimi (Moonshot)",
        auth_kind: AuthKind::ApiKey,
        detail: "MOONSHOT_API_KEY, Moonshot open platform",
        recommended: false,
        order: 4,
        aliases: &["kimi", "kimi-api", "moonshot-ai"],
    },
    LoginProvider {
        id: "kimi-code",
        display_name: "Kimi Code",
        auth_kind: AuthKind::ApiKey,
        detail: "KIMI_API_KEY, Kimi Code coding plan",
        recommended: false,
        order: 5,
        aliases: &["kimi-coding", "kimi-for-coding"],
    },
    LoginProvider {
        id: "zai",
        display_name: "Z.AI (GLM)",
        auth_kind: AuthKind::ApiKey,
        detail: "ZAI_API_KEY, pay-per-token GLM API",
        recommended: false,
        order: 6,
        aliases: &["z.ai", "zhipu", "glm"],
    },
    LoginProvider {
        id: "zai-coding",
        display_name: "Z.AI Coding Plan",
        auth_kind: AuthKind::ApiKey,
        detail: "ZAI_API_KEY, GLM coding plan subscription",
        recommended: false,
        order: 7,
        aliases: &["glm-coding", "zai-coding-plan"],
    },
    LoginProvider {
        id: "opencode-zen",
        display_name: "OpenCode Zen",
        auth_kind: AuthKind::ApiKey,
        detail: "OPENCODE_API_KEY, curated pay-per-token gateway",
        recommended: false,
        order: 8,
        aliases: &["opencode", "zen"],
    },
    LoginProvider {
        id: "opencode-go",
        display_name: "OpenCode Go",
        auth_kind: AuthKind::ApiKey,
        detail: "OPENCODE_API_KEY, OpenCode Go subscription",
        recommended: false,
        order: 9,
        aliases: &["opencodego"],
    },
    LoginProvider {
        id: "gemini",
        display_name: "Google Gemini",
        auth_kind: AuthKind::ApiKey,
        detail: "GEMINI_API_KEY, Google AI Studio",
        recommended: false,
        order: 10,
        aliases: &["gemini-api", "google", "google-ai-studio"],
    },
    LoginProvider {
        id: "devin",
        display_name: "Devin",
        auth_kind: AuthKind::OAuth,
        detail: "Devin / Cognition account",
        recommended: false,
        order: 11,
        aliases: &["cognition"],
    },
    LoginProvider {
        id: "ollama",
        display_name: "Ollama",
        auth_kind: AuthKind::Local,
        detail: "Local Ollama instance",
        recommended: false,
        order: 12,
        aliases: &[],
    },
    LoginProvider {
        id: "lmstudio",
        display_name: "LM Studio",
        auth_kind: AuthKind::Local,
        detail: "Local LM Studio server",
        recommended: false,
        order: 13,
        aliases: &["lm-studio"],
    },
    LoginProvider {
        id: "custom",
        display_name: "Custom",
        auth_kind: AuthKind::ApiKey,
        detail: "Any OpenAI-compatible base_url",
        recommended: false,
        order: 14,
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
