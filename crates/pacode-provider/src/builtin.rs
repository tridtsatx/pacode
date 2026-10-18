//! Builtin provider table: the providers pacode knows without any `config.toml` entry.
//!
//! Every entry is synthesized into the registry when its credential is present (a
//! stored login or its API-key environment variable), so `export OPENROUTER_API_KEY=...`
//! or `/login moonshot` is all it takes. A `[providers.<id>]` table in the config
//! replaces the builtin of the same id wholesale. The wire quirks of each endpoint
//! live in its [`Compat`], never in `if provider == ...` branches.

use pacode_auth::catalog::AuthKind;
use pacode_types::{ModelConfig, ProviderConfig, ProviderKind};

use crate::compat::{Compat, MaxTokensField, ReasoningWire, ToolIdRule};

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod builtin_tests;

/// A model shipped in the builtin table (shown even when `/models` is unreachable).
#[derive(Clone, Copy, Debug)]
pub struct BuiltinModel {
    pub id: &'static str,
    pub name: &'static str,
    pub context_window: Option<u32>,
}

/// One builtin provider.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinProvider {
    /// Registry key and `/login` id.
    pub id: &'static str,
    pub display_name: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    /// Hosts that identify this endpoint family for [`Compat::for_base_url`].
    pub hosts: &'static [&'static str],
    /// Environment variable holding the API key.
    pub api_key_env: Option<&'static str>,
    pub auth: AuthKind,
    /// Fetch `GET /models` in addition to `models`.
    pub catalog: bool,
    pub models: &'static [BuiltinModel],
    pub compat: Compat,
}

impl BuiltinProvider {
    /// The `ProviderConfig` this entry stands for.
    pub fn to_config(&self) -> ProviderConfig {
        ProviderConfig {
            kind: self.kind,
            base_url: self.base_url.to_string(),
            api_key_env: self.api_key_env.map(String::from),
            catalog: self.catalog,
            models: self
                .models
                .iter()
                .map(|m| ModelConfig {
                    id: m.id.to_string(),
                    display_name: Some(m.name.to_string()),
                    context_window: m.context_window,
                    reasoning: None,
                })
                .collect(),
            ..ProviderConfig::default()
        }
    }

    /// The API key from the environment, when the variable is set and non-blank.
    pub fn env_key(&self) -> Option<String> {
        let name = self.api_key_env?;
        let value = std::env::var(name).ok()?;
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }
}

const fn model(id: &'static str, name: &'static str, ctx: Option<u32>) -> BuiltinModel {
    BuiltinModel {
        id,
        name,
        context_window: ctx,
    }
}

const OPENAI_COMPAT: Compat = Compat {
    max_tokens_field: MaxTokensField::MaxCompletionTokens,
    reasoning_hints: Some(&["o1", "o3", "o4", "gpt-5"]),
    reasoning_overrides: &[("gpt-5-chat", ReasoningWire::Unsupported)],
    tool_id: ToolIdRule {
        max_len: Some(40),
        safe_charset: true,
    },
    ..Compat::DEFAULT
};

const OPENROUTER_COMPAT: Compat = Compat {
    reasoning: ReasoningWire::ReasoningObject,
    // Anthropic and Bedrock upstreams validate ids against `^[a-zA-Z0-9_-]+$`.
    tool_id: ToolIdRule {
        max_len: None,
        safe_charset: true,
    },
    ..Compat::DEFAULT
};

/// Moonshot and Kimi Code. K2.x only toggles thinking; K3 takes a tiered effort.
/// Moonshot meters tokens-per-minute on `max_tokens`, so a limit is always sent, and
/// rejects thinking tool-call turns that lack `reasoning_content` or text.
const KIMI_COMPAT: Compat = Compat {
    default_max_tokens: Some(32_768),
    reasoning: ReasoningWire::ThinkingToggle,
    reasoning_overrides: &[("k3", ReasoningWire::ReasoningEffortTiered)],
    reasoning_hints: Some(&[
        "kimi-k2.5",
        "kimi-k2.6",
        "kimi-k2.7",
        "kimi-k3",
        "kimi-for",
        "thinking",
    ]),
    reasoning_content_for_tool_calls: true,
    text_for_tool_calls: true,
    ..Compat::DEFAULT
};

const ZAI_COMPAT: Compat = Compat {
    reasoning: ReasoningWire::ThinkingToggle,
    reasoning_hints: Some(&["glm-4.5", "glm-4.6", "glm-4.7", "glm-5"]),
    ..Compat::DEFAULT
};

const OPENCODE_COMPAT: Compat = Compat {
    reasoning_content_for_tool_calls: true,
    ..Compat::DEFAULT
};

/// Gemini's OpenAI layer lists models as `models/<id>`, accepts `reasoning_effort` only
/// for thinking-capable generations, and rejects some JSON-schema keywords.
const GEMINI_COMPAT: Compat = Compat {
    reasoning_hints: Some(&["gemini-2.5", "gemini-3"]),
    strip_schema_keys: &["$schema", "additionalProperties"],
    strip_model_prefix: Some("models/"),
    ..Compat::DEFAULT
};

static BUILTIN_PROVIDERS: &[BuiltinProvider] = &[
    BuiltinProvider {
        id: "openai",
        display_name: "OpenAI (ChatGPT login)",
        kind: ProviderKind::Codex,
        base_url: "https://api.openai.com/v1",
        hosts: &[],
        api_key_env: None,
        auth: AuthKind::OAuth,
        catalog: true,
        models: &[],
        compat: Compat::DEFAULT,
    },
    BuiltinProvider {
        id: "openai-api",
        display_name: "OpenAI API",
        kind: ProviderKind::OpenAi,
        base_url: "https://api.openai.com/v1",
        hosts: &["api.openai.com"],
        api_key_env: Some("OPENAI_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[model("gpt-5.5", "GPT-5.5", None)],
        compat: OPENAI_COMPAT,
    },
    BuiltinProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        kind: ProviderKind::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        hosts: &["openrouter.ai"],
        api_key_env: Some("OPENROUTER_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[model("openai/gpt-5.5", "GPT-5.5", None)],
        compat: OPENROUTER_COMPAT,
    },
    BuiltinProvider {
        id: "moonshot",
        display_name: "Kimi (Moonshot)",
        kind: ProviderKind::OpenAi,
        base_url: "https://api.moonshot.ai/v1",
        hosts: &["api.moonshot.ai", "api.moonshot.cn"],
        api_key_env: Some("MOONSHOT_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[
            model("kimi-k2.7-code", "Kimi K2.7 Code", None),
            model("kimi-k3", "Kimi K3", Some(1_000_000)),
        ],
        compat: KIMI_COMPAT,
    },
    BuiltinProvider {
        id: "kimi-code",
        display_name: "Kimi Code",
        kind: ProviderKind::OpenAi,
        base_url: "https://api.kimi.com/coding/v1",
        hosts: &["api.kimi.com"],
        api_key_env: Some("KIMI_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[model("kimi-for-coding", "Kimi for Coding", None)],
        compat: KIMI_COMPAT,
    },
    BuiltinProvider {
        id: "zai",
        display_name: "Z.AI (GLM)",
        kind: ProviderKind::OpenAi,
        base_url: "https://api.z.ai/api/paas/v4",
        hosts: &["api.z.ai", "open.bigmodel.cn"],
        api_key_env: Some("ZAI_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[model("glm-5.3", "GLM-5.3", Some(1_000_000))],
        compat: ZAI_COMPAT,
    },
    BuiltinProvider {
        id: "zai-coding",
        display_name: "Z.AI Coding Plan",
        kind: ProviderKind::OpenAi,
        base_url: "https://api.z.ai/api/coding/paas/v4",
        hosts: &["api.z.ai", "open.bigmodel.cn"],
        api_key_env: Some("ZAI_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[
            model("glm-5.3", "GLM-5.3", Some(1_000_000)),
            model("glm-5.3-flash", "GLM-5.3 Flash", Some(1_000_000)),
        ],
        compat: ZAI_COMPAT,
    },
    BuiltinProvider {
        id: "opencode-zen",
        display_name: "OpenCode Zen",
        kind: ProviderKind::OpenAi,
        base_url: "https://opencode.ai/zen/v1",
        hosts: &["opencode.ai"],
        api_key_env: Some("OPENCODE_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[],
        compat: OPENCODE_COMPAT,
    },
    BuiltinProvider {
        id: "opencode-go",
        display_name: "OpenCode Go",
        kind: ProviderKind::OpenAi,
        base_url: "https://opencode.ai/zen/go/v1",
        hosts: &["opencode.ai"],
        api_key_env: Some("OPENCODE_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[model("kimi-k2.7-code", "Kimi K2.7 Code", None)],
        compat: OPENCODE_COMPAT,
    },
    BuiltinProvider {
        id: "gemini",
        display_name: "Google Gemini",
        kind: ProviderKind::OpenAi,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        hosts: &["generativelanguage.googleapis.com"],
        api_key_env: Some("GEMINI_API_KEY"),
        auth: AuthKind::ApiKey,
        catalog: true,
        models: &[
            model(
                "gemini-3.1-pro-preview",
                "Gemini 3.1 Pro Preview",
                Some(1_048_576),
            ),
            model("gemini-2.5-pro", "Gemini 2.5 Pro", Some(1_048_576)),
            model("gemini-2.5-flash", "Gemini 2.5 Flash", Some(1_048_576)),
        ],
        compat: GEMINI_COMPAT,
    },
    BuiltinProvider {
        id: "ollama",
        display_name: "Ollama",
        kind: ProviderKind::OpenAi,
        base_url: "http://localhost:11434/v1",
        hosts: &[],
        api_key_env: None,
        auth: AuthKind::Local,
        catalog: true,
        models: &[],
        compat: Compat::DEFAULT,
    },
    BuiltinProvider {
        id: "lmstudio",
        display_name: "LM Studio",
        kind: ProviderKind::OpenAi,
        base_url: "http://localhost:1234/v1",
        hosts: &[],
        api_key_env: None,
        auth: AuthKind::Local,
        catalog: true,
        models: &[],
        compat: Compat::DEFAULT,
    },
];

/// Every builtin provider, in `/login` picker order.
pub fn builtin_providers() -> &'static [BuiltinProvider] {
    BUILTIN_PROVIDERS
}

/// The builtin with registry id `id`.
pub fn find_builtin(id: &str) -> Option<&'static BuiltinProvider> {
    BUILTIN_PROVIDERS.iter().find(|b| b.id == id)
}
