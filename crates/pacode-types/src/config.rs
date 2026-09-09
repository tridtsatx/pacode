//! Configuration structures (`~/.config/pacode/config.toml`). Loading lives in
//! `pacode-config`; this module only defines the shape and defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Effort, ModelRoute, Pricing};
use crate::state::Mode;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderDefaults,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Keyed by model id (without provider prefix).
    pub pricing: BTreeMap<String, Pricing>,
    pub ui: UiConfig,
    pub exec: ExecConfig,
    pub agents: AgentsConfig,
    pub daemon: DaemonConfig,
    pub context: ContextConfig,
    pub permissions: PermissionsConfig,
    pub mcp: McpConfig,
    pub session: SessionConfig,
    pub plugins: PluginsConfig,
}

impl Config {
    /// Default route from `[provider].default`, parsed against configured providers.
    pub fn default_route(&self) -> Option<ModelRoute> {
        let raw = self.provider.default.as_deref()?;
        let known = self.providers.keys().map(String::as_str);
        let fallback = self.providers.keys().next().map(String::as_str);
        ModelRoute::parse(raw, known, fallback)
    }

    pub fn pricing_for(&self, model: &str) -> Option<Pricing> {
        self.pricing.get(model).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDefaults {
    /// `provider/model`.
    pub default: Option<String>,
    pub effort: Effort,
    /// Base idle timeout of a streaming response, scaled by effort.
    pub stream_idle_secs: u64,
    pub max_retries: u32,
}

impl Default for ProviderDefaults {
    fn default() -> Self {
        Self {
            default: None,
            effort: Effort::Medium,
            stream_idle_secs: 180,
            max_retries: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ProviderConfig {
    /// e.g. `https://api.example.com/v1`
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    /// Fetch `GET /models` for the picker.
    pub catalog: bool,
    pub models: Vec<ModelConfig>,
    /// Default context window for models of this provider when unknown.
    pub context_window: Option<u32>,
    /// Send `reasoning_effort`. `None` = auto (send for models flagged as reasoning).
    pub reasoning: Option<bool>,
    /// Override the wire value per effort, e.g. `{ max = "xhigh" }`.
    pub effort_map: BTreeMap<String, String>,
    /// Merged into every request body.
    pub extra_body: Option<Value>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u32>,
    pub reasoning: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub hints: UiHints,
    pub ascii_only: bool,
    pub mouse: bool,
    pub auto_copy: bool,
    /// `auto` (COLORTERM detection), `truecolor`, or `ansi`.
    pub color: String,
    /// Max transcript cells kept in the client before older ones are evicted.
    pub transcript_cells: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            hints: UiHints::default(),
            ascii_only: false,
            mouse: true,
            auto_copy: true,
            color: "auto".to_string(),
            transcript_cells: 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiHints {
    pub model: bool,
    pub effort: bool,
}

impl Default for UiHints {
    fn default() -> Self {
        Self {
            model: false,
            effort: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecConfig {
    /// Foreground wait before a command is handed to the background.
    pub yield_after_secs: u64,
    /// Silence before a stall notification.
    pub stall_secs: u64,
    pub max_spool_bytes: u64,
    /// In-RAM tail per task.
    pub tail_bytes: usize,
    pub kill_grace_secs: u64,
    pub max_tasks: usize,
    /// Hard cap for a foreground command without `background`.
    pub default_timeout_secs: u64,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            yield_after_secs: 10,
            stall_secs: 120,
            max_spool_bytes: 50 * 1024 * 1024,
            tail_bytes: 64 * 1024,
            kill_grace_secs: 5,
            max_tasks: 64,
            default_timeout_secs: 3600,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub max_live: usize,
    /// `provider/model` for subagents when the spawn call does not name one.
    pub default_model: Option<String>,
    pub default_effort: Option<Effort>,
    /// Max tool-calling turns for one subagent before it is stopped.
    pub max_turns: u32,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            max_live: 8,
            default_model: None,
            default_effort: None,
            max_turns: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub idle_timeout_secs: u64,
    pub socket: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 600,
            socket: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Fraction of the context window that triggers compaction.
    pub compaction_threshold: f32,
    pub tool_output_cap_chars: usize,
    pub injection_cap_chars: usize,
    /// Cap for AGENTS.md / CLAUDE.md content in the system prompt.
    pub instructions_cap_chars: usize,
    pub memory_cap_chars: usize,
    pub compaction_model: Option<String>,
    /// Recent messages kept verbatim after compaction.
    pub keep_recent_messages: usize,
    /// Used when neither the provider nor the config knows the window.
    pub default_context_window: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: 0.85,
            tool_output_cap_chars: 16_000,
            injection_cap_chars: 4_000,
            instructions_cap_chars: 32_000,
            memory_cap_chars: 8_000,
            compaction_model: None,
            keep_recent_messages: 6,
            default_context_window: 128_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    pub default_mode: Mode,
    pub allow_catastrophic: bool,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default_mode: Mode::Build,
            allow_catastrophic: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
    pub sampling: bool,
    pub sampling_max_tokens: u32,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: BTreeMap::new(),
            sampling: true,
            sampling_max_tokens: 2048,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub enabled: bool,
    /// Start on first tool call instead of at session start.
    pub lazy: bool,
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            lazy: true,
            timeout_secs: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Model used to name the session after the first reply; default: session model.
    pub title_model: Option<String>,
    /// Transcript items sent in a snapshot / history page.
    pub history_page: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            title_model: None,
            history_page: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub enabled: bool,
    /// Empty means default `~/.config/pacode/plugins`.
    pub dirs: Vec<PathBuf>,
    pub wasm_memory_mb: u32,
    pub lua_memory_mb: u32,
    pub hook_timeout_ms: u64,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
            wasm_memory_mb: 64,
            lua_memory_mb: 32,
            hook_timeout_ms: 5000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_gives_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.exec.yield_after_secs, 10);
        assert_eq!(cfg.permissions.default_mode, Mode::Build);
        assert!(!cfg.ui.hints.model);
        assert!(cfg.default_route().is_none());
    }

    #[test]
    fn default_route_uses_known_providers() {
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "provider": { "default": "bubna/gemini-3.8-flash" },
            "providers": { "bubna": { "base_url": "https://x/v1" } }
        }))
        .unwrap();
        let route = cfg.default_route().unwrap();
        assert_eq!(route.provider, "bubna");
        assert_eq!(route.model, "gemini-3.8-flash");
    }
}
