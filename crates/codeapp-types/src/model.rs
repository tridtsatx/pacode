//! Model routes, effort levels, pricing and catalog entries.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::stream::Usage;

/// Reasoning effort. Shown as the tail of the model in the footer (`Gemini 3.8 Flash | max`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Effort::Low),
            "medium" | "med" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" | "x-high" | "extra-high" => Some(Effort::XHigh),
            "max" => Some(Effort::Max),
            _ => None,
        }
    }

    /// Multiplier for stream idle timeouts: silent reasoning takes longer at high efforts.
    pub fn idle_timeout_factor(self) -> u64 {
        match self {
            Effort::Low | Effort::Medium => 1,
            Effort::High => 2,
            Effort::XHigh => 3,
            Effort::Max => 4,
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `provider/model`. The provider is a key in `[providers.<key>]` of the config.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRoute {
    pub provider: String,
    pub model: String,
}

impl ModelRoute {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// Parse `provider/model`. The split is at the FIRST `/` only when the prefix is a
    /// known provider; otherwise the whole string is the model on `default_provider`.
    pub fn parse<'a>(
        s: &str,
        known_providers: impl IntoIterator<Item = &'a str>,
        default_provider: Option<&str>,
    ) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Some((prefix, rest)) = s.split_once('/')
            && !rest.is_empty()
            && known_providers.into_iter().any(|p| p == prefix)
        {
            return Some(Self::new(prefix, rest));
        }
        default_provider.map(|p| Self::new(p, s))
    }

    /// Lossy parse used by config/CLI before the registry is known: always splits at
    /// the first `/`.
    pub fn parse_lossy(s: &str) -> Option<Self> {
        let (provider, model) = s.trim().split_once('/')?;
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self::new(provider, model))
    }

    /// Human label for the footer: `gemini-3.8-flash` → `Gemini 3.8 Flash`.
    pub fn display_name(&self) -> String {
        prettify_model_name(&self.model)
    }
}

impl fmt::Display for ModelRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// `gemini-3.8-flash` → `Gemini 3.8 Flash`, `gpt-oss-120b` → `Gpt Oss 120b`,
/// `qwen3.5:397b` → `Qwen3.5 397b`. Vendor prefixes (`z-ai/`) are dropped.
pub fn prettify_model_name(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    base.split(['-', '_', ':'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) if first.is_ascii_alphabetic() => {
                    first.to_ascii_uppercase().to_string() + chars.as_str()
                }
                Some(first) => first.to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// USD per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pricing {
    pub input_per_m: f64,
    pub output_per_m: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_per_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_per_m: Option<f64>,
}

impl Pricing {
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let m = 1_000_000.0;
        let uncached_input = usage.input_tokens.saturating_sub(usage.cache_read_tokens);
        let mut cost = uncached_input as f64 / m * self.input_per_m
            + (usage.output_tokens + usage.reasoning_tokens) as f64 / m * self.output_per_m;
        cost +=
            usage.cache_read_tokens as f64 / m * self.cache_read_per_m.unwrap_or(self.input_per_m);
        cost += usage.cache_write_tokens as f64 / m * self.cache_write_per_m.unwrap_or(0.0);
        cost
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub route: ModelRoute,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parse_respects_known_providers() {
        let known = ["bubna", "ollama"];
        let r = ModelRoute::parse("bubna/gemini-3.8-flash", known, Some("ollama")).unwrap();
        assert_eq!(r.provider, "bubna");
        assert_eq!(r.model, "gemini-3.8-flash");
        let r = ModelRoute::parse("z-ai/glm-4.6", known, Some("ollama")).unwrap();
        assert_eq!(r.provider, "ollama");
        assert_eq!(r.model, "z-ai/glm-4.6");
        assert!(ModelRoute::parse("glm", known, None).is_none());
    }

    #[test]
    fn pretty_names() {
        assert_eq!(prettify_model_name("gemini-3.8-flash"), "Gemini 3.8 Flash");
        assert_eq!(prettify_model_name("z-ai/glm-4.6"), "Glm 4.6");
        assert_eq!(prettify_model_name("qwen3.5:397b"), "Qwen3.5 397b");
    }

    #[test]
    fn pricing_cost() {
        let p = Pricing {
            input_per_m: 1.0,
            output_per_m: 2.0,
            cache_read_per_m: Some(0.1),
            cache_write_per_m: None,
        };
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            reasoning_tokens: 0,
            cache_read_tokens: 500_000,
            cache_write_tokens: 0,
        };
        let cost = p.cost_usd(&usage);
        assert!((cost - (0.5 + 1.0 + 0.05)).abs() < 1e-9);
    }
}
