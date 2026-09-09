//! Configuration structures (`~/.config/pacode/config.toml`). Loading lives in
//! `pacode-config`; this module only defines the shape and defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use serde_json;
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
    pub skills: SkillsConfig,
    pub theme: ThemeConfig,
    pub font: FontConfig,
    pub keys: KeysConfig,
    pub web: WebConfig,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ups {
    Fixed(u16),
    Dynamic,
    Auto,
}

impl Default for Ups {
    fn default() -> Self {
        Self::Fixed(10)
    }
}

impl Ups {
    pub fn frame_interval(&self, detected_hz: Option<u16>) -> Option<Duration> {
        match *self {
            Self::Dynamic => None,
            Self::Fixed(n) => {
                let clamped = n.clamp(1, 240);
                Some(Duration::from_millis(1000 / clamped as u64))
            }
            Self::Auto => {
                let hz = detected_hz.unwrap_or(10);
                let clamped = hz.clamp(1, 240);
                Some(Duration::from_millis(1000 / clamped as u64))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum UpsHelper {
    Fixed(u16),
    Named(UpsNamed),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UpsNamed {
    Dynamic,
    Auto,
}

impl Serialize for Ups {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let helper = match *self {
            Self::Fixed(n) => UpsHelper::Fixed(n),
            Self::Dynamic => UpsHelper::Named(UpsNamed::Dynamic),
            Self::Auto => UpsHelper::Named(UpsNamed::Auto),
        };
        helper.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Ups {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let helper = UpsHelper::deserialize(deserializer)?;
        let ups = match helper {
            UpsHelper::Fixed(n) => Self::Fixed(n),
            UpsHelper::Named(UpsNamed::Dynamic) => Self::Dynamic,
            UpsHelper::Named(UpsNamed::Auto) => Self::Auto,
        };
        Ok(ups)
    }
}

fn default_images() -> String {
    "auto".to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub hints: UiHints,
    pub ascii_only: bool,
    pub mouse: bool,
    pub auto_copy: bool,
    #[serde(default)]
    pub ups: Ups,
    /// `auto` (COLORTERM detection), `truecolor`, or `ansi`.
    pub color: String,
    /// Max transcript cells kept in the client before older ones are evicted.
    pub transcript_cells: usize,
    /// `auto` or `off`.
    #[serde(default = "default_images")]
    pub images: String,
    /// Vim keybindings for prompt input (`[ui] vim = true`).
    #[serde(default)]
    pub vim: bool,
    /// Show the model's reasoning line in the transcript. Off by default: the
    /// reasoning is still recorded, it is just not drawn.
    #[serde(default)]
    pub thinking: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            hints: UiHints::default(),
            ascii_only: false,
            mouse: true,
            auto_copy: true,
            ups: Ups::default(),
            color: "auto".to_string(),
            transcript_cells: 500,
            images: default_images(),
            vim: false,
            thinking: false,
        }
    }
}

impl UiConfig {
    pub fn images_enabled(&self) -> bool {
        self.images != "off"
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
    pub idle_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: BTreeMap::new(),
            sampling: true,
            sampling_max_tokens: 2048,
            idle_timeout_secs: 300,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub enabled: bool,
    /// Empty means default `<config dir>/skills`.
    pub dirs: Vec<PathBuf>,
    pub max_body_bytes: usize,
    pub max_listed: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
            max_body_bytes: 16384,
            max_listed: 100,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Default number of results returned by websearch if unspecified.
    pub default_num_results: usize,
    /// Request timeout in seconds for web tools.
    pub request_timeout_secs: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            default_num_results: 8,
            request_timeout_secs: 15,
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
        assert_eq!(cfg.ui.ups, Ups::Fixed(10));
        assert_eq!(cfg.ui.images, "auto");
        assert!(cfg.ui.images_enabled());
        assert!(!cfg.ui.vim);
        assert!(cfg.skills.enabled);
        assert!(cfg.skills.dirs.is_empty());
        assert_eq!(cfg.skills.max_body_bytes, 16384);
        assert_eq!(cfg.skills.max_listed, 100);
        assert_eq!(cfg.web.default_num_results, 8);
        assert_eq!(cfg.web.request_timeout_secs, 15);
        assert_eq!(cfg.mcp.idle_timeout_secs, 300);
        assert!(cfg.default_route().is_none());
    }

    #[test]
    fn ui_images_off() {
        let cfg: Config = serde_json::from_str(r#"{"ui": {"images": "off"}}"#).unwrap();
        assert_eq!(cfg.ui.images, "off");
        assert!(!cfg.ui.images_enabled());
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

    #[test]
    fn ups_serde_and_clamping() {
        let fixed: Ups = serde_json::from_str("10").unwrap();
        assert_eq!(fixed, Ups::Fixed(10));
        assert_eq!(fixed.frame_interval(None), Some(Duration::from_millis(100)));

        let dyn_val: Ups = serde_json::from_str("\"dynamic\"").unwrap();
        assert_eq!(dyn_val, Ups::Dynamic);
        assert_eq!(dyn_val.frame_interval(Some(60)), None);

        let auto_val: Ups = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(auto_val, Ups::Auto);
        assert_eq!(
            auto_val.frame_interval(None),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            auto_val.frame_interval(Some(60)),
            Some(Duration::from_millis(16))
        );

        // Clamping tests
        assert_eq!(
            Ups::Fixed(0).frame_interval(None),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            Ups::Fixed(300).frame_interval(None),
            Some(Duration::from_millis(4))
        );
        assert_eq!(
            Ups::Auto.frame_interval(Some(0)),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            Ups::Auto.frame_interval(Some(300)),
            Some(Duration::from_millis(4))
        );

        // Symmetric serialization
        assert_eq!(serde_json::to_string(&Ups::Fixed(10)).unwrap(), "10");
        assert_eq!(serde_json::to_string(&Ups::Dynamic).unwrap(), "\"dynamic\"");
        assert_eq!(serde_json::to_string(&Ups::Auto).unwrap(), "\"auto\"");
    }
}

/// `[theme]`: which palette to use. `name` is a built-in id or the stem of a
/// `<config dir>/themes/<name>.toml` file; `overrides` patches individual roles
/// on top of it, so a user can tweak one colour without copying a whole theme.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: String,
    /// Role name (`accent`, `green`, ...) to colour (`#rrggbb`, `red`, `9`).
    pub overrides: BTreeMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "pacode-dark".to_string(),
            overrides: BTreeMap::new(),
        }
    }
}

/// `[font]`: applied best effort. A terminal application does not own its font;
/// only some emulators expose a control sequence for it, so every field here is
/// a request that may be silently unavailable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: Option<String>,
    pub size: Option<u16>,
    /// `normal` or `bold`; only affects what pacode draws itself.
    pub weight: Option<String>,
}

/// `[keys]`: action name to key binding, e.g. `follow_agent = "alt+f"`.
/// Unset actions keep their built-in binding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    #[serde(flatten)]
    pub bindings: BTreeMap<String, String>,
}
