//! Per-endpoint wire quirks of OpenAI-compatible providers.
//!
//! One [`Compat`] value describes everything a given endpoint does differently from
//! plain OpenAI `/chat/completions`: the max-token field name, how reasoning is
//! requested, tool-call id restrictions, JSON-schema keywords it rejects, and so on.
//! [`crate::OpenAiCompat`] resolves it from the base URL host, so a user's own config
//! entry pointing at e.g. `api.moonshot.ai` gets the same treatment as the builtin
//! entry. Unknown hosts (Ollama, LM Studio, custom gateways) get [`Compat::DEFAULT`].

use pacode_types::Effort;
use serde_json::Value;

#[cfg(test)]
#[path = "compat_tests.rs"]
mod compat_tests;

/// Placeholder sent where a provider insists on non-empty text / reasoning that
/// pacode does not replay.
pub(crate) const PLACEHOLDER: &str = ".";

pub(crate) const DEFAULT_REASONING_KEYWORDS: &[&str] = &[
    "gemini",
    "o1",
    "o3",
    "o4",
    "gpt-5",
    "deepseek-r",
    "qwq",
    "thinking",
    "reason",
    "glm-5",
    "kimi-k2",
    "minimax",
];

/// Name of the output-token limit field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxTokensField {
    /// `max_tokens` (everything except official OpenAI).
    MaxTokens,
    /// `max_completion_tokens` (official OpenAI; o-series and gpt-5 reject `max_tokens`).
    MaxCompletionTokens,
}

impl MaxTokensField {
    pub fn key(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// How a reasoning level is put on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningWire {
    /// The endpoint takes no reasoning control; never send one.
    Unsupported,
    /// `"reasoning_effort": "<level>"` (OpenAI, Gemini).
    ReasoningEffort,
    /// `"reasoning_effort"` on Kimi K3's three-step scale (`low` / `high` / `max`).
    ReasoningEffortTiered,
    /// `"reasoning": {"effort": "<level>"}` (OpenRouter).
    ReasoningObject,
    /// Binary `"thinking": {"type": "enabled"}` (Z.AI GLM, Moonshot K2.x).
    ThinkingToggle,
}

/// Restrictions on tool-call ids echoed back in history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolIdRule {
    /// Keep only the last `max_len` characters.
    pub max_len: Option<usize>,
    /// Replace everything outside `[A-Za-z0-9_-]` with `_`.
    pub safe_charset: bool,
}

/// Wire quirks of one endpoint family.
#[derive(Clone, Copy, Debug)]
pub struct Compat {
    pub max_tokens_field: MaxTokensField,
    /// Sent when the request carries no limit (Kimi meters TPM on `max_tokens`).
    pub default_max_tokens: Option<u32>,
    pub reasoning: ReasoningWire,
    /// `(model id substring, wire)` pairs checked before [`Compat::reasoning`].
    pub reasoning_overrides: &'static [(&'static str, ReasoningWire)],
    /// Substrings marking reasoning-capable models; `None` = built-in heuristic.
    pub reasoning_hints: Option<&'static [&'static str]>,
    pub tool_id: ToolIdRule,
    /// JSON-schema keywords removed from tool parameters.
    pub strip_schema_keys: &'static [&'static str],
    /// Prefix the `/models` listing adds to ids (`models/` on Gemini).
    pub strip_model_prefix: Option<&'static str>,
    /// Assistant tool-call turns must carry `reasoning_content` while thinking is on.
    pub reasoning_content_for_tool_calls: bool,
    /// Assistant tool-call turns must carry non-empty text.
    pub text_for_tool_calls: bool,
}

impl Compat {
    pub const DEFAULT: Compat = Compat {
        max_tokens_field: MaxTokensField::MaxTokens,
        default_max_tokens: None,
        reasoning: ReasoningWire::ReasoningEffort,
        reasoning_overrides: &[],
        reasoning_hints: None,
        tool_id: ToolIdRule {
            max_len: None,
            safe_charset: false,
        },
        strip_schema_keys: &[],
        strip_model_prefix: None,
        reasoning_content_for_tool_calls: false,
        text_for_tool_calls: false,
    };

    /// Compat for the endpoint behind `base_url`, matched on its host.
    pub fn for_base_url(base_url: &str) -> Compat {
        let host = host_of(base_url);
        crate::builtin::builtin_providers()
            .iter()
            .find(|b| b.hosts.iter().any(|h| *h == host))
            .map(|b| b.compat)
            .unwrap_or(Compat::DEFAULT)
    }

    pub fn reasoning_wire(&self, model: &str) -> ReasoningWire {
        let lower = model.to_ascii_lowercase();
        self.reasoning_overrides
            .iter()
            .find(|(needle, _)| lower.contains(*needle))
            .map(|(_, wire)| *wire)
            .unwrap_or(self.reasoning)
    }

    /// Whether `model` is assumed to support reasoning when nothing is configured.
    pub fn heuristic_reasoning(&self, model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        let hints = self.reasoning_hints.unwrap_or(DEFAULT_REASONING_KEYWORDS);
        hints.iter().any(|k| lower.contains(*k))
    }

    /// Apply the tool-call id restrictions. Deterministic, so the id on the assistant
    /// message and on the matching tool result stay equal.
    pub fn tool_id(&self, id: &str) -> String {
        let mut out: String = if self.tool_id.safe_charset {
            id.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        } else {
            id.to_string()
        };
        if let Some(max) = self.tool_id.max_len {
            let count = out.chars().count();
            if count > max {
                out = out.chars().skip(count - max).collect();
            }
        }
        out
    }

    /// Tool parameter schema with the rejected keywords removed.
    pub fn clean_schema(&self, schema: &Value) -> Value {
        if self.strip_schema_keys.is_empty() {
            return schema.clone();
        }
        let mut cleaned = schema.clone();
        strip_keys(&mut cleaned, self.strip_schema_keys);
        cleaned
    }

    /// Wire string for `effort` on `wire`, before any user `effort_map` override.
    pub fn effort_str(wire: ReasoningWire, effort: Effort) -> &'static str {
        match wire {
            ReasoningWire::ReasoningEffortTiered => match effort {
                Effort::Low => "low",
                Effort::Medium | Effort::High => "high",
                Effort::XHigh | Effort::Max => "max",
            },
            ReasoningWire::Unsupported
            | ReasoningWire::ReasoningEffort
            | ReasoningWire::ReasoningObject
            | ReasoningWire::ThinkingToggle => match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High | Effort::XHigh | Effort::Max => "high",
            },
        }
    }

    /// Model id as pacode should refer to it (listing prefix removed).
    pub fn model_id<'a>(&self, id: &'a str) -> &'a str {
        match self.strip_model_prefix {
            Some(prefix) => id.strip_prefix(prefix).unwrap_or(id),
            None => id,
        }
    }
}

impl Default for Compat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn strip_keys(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in keys {
                map.remove(*key);
            }
            for child in map.values_mut() {
                strip_keys(child, keys);
            }
        }
        Value::Array(items) => {
            for child in items.iter_mut() {
                strip_keys(child, keys);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Lower-cased host of `url` without scheme, credentials, port or path.
pub(crate) fn host_of(url: &str) -> String {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    let host = authority.split(':').next().unwrap_or("");
    host.to_ascii_lowercase()
}
