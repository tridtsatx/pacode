//! Hand-written table describing every settable field of `Config`.
//!
//! Provides validation, read-back, and mutation helpers for `/config`.

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;

use std::path::PathBuf;

use pacode_types::model::Effort;
use pacode_types::state::Mode;
use pacode_types::{Config, Ups};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    Bool,
    Integer { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    String,
    Enum { options: &'static [&'static str] },
    Action { command: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingEntry {
    pub dotted_key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub section: &'static str,
    pub default_value: &'static str,
    pub kind: SettingKind,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("unknown setting: {key}")]
    UnknownKey { key: String },
    #[error("invalid boolean '{value}': expected 'true' or 'false'")]
    InvalidBool { value: String },
    #[error("invalid integer '{value}': {source}")]
    InvalidInteger {
        value: String,
        source: std::num::ParseIntError,
    },
    #[error("integer {value} out of range ({min}..={max})")]
    IntegerOutOfRange { value: i64, min: i64, max: i64 },
    #[error("invalid float '{value}': {source}")]
    InvalidFloat {
        value: String,
        source: std::num::ParseFloatError,
    },
    #[error("float {value} out of range ({min}..={max})")]
    FloatOutOfRange { value: f64, min: f64, max: f64 },
    #[error("invalid option '{value}': expected one of {}", options.join(", "))]
    InvalidOption { value: String, options: Vec<String> },
    #[error("invalid updates per second '{value}': expected 'auto', 'dynamic', or 1..=240")]
    InvalidUps { value: String },
    #[error("setting '{key}' cannot be edited directly; use {command}")]
    NotDirectlyEditable { key: String, command: String },
}

/// Explicit, named skip list for collection-valued fields that cannot be edited in a simple list UI.
pub const SKIP_LIST: &[&str] = &["providers", "pricing", "plugins.dirs", "skills.dirs"];

pub const SECTIONS: &[&str] = &[
    "provider",
    "ui",
    "exec",
    "agents",
    "daemon",
    "context",
    "permissions",
    "mcp",
    "session",
    "plugins",
    "skills",
    "theme",
    "font",
    "keys",
    "web",
];

const EFFORT_OPTIONS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const SUBAGENT_EFFORT_OPTIONS: &[&str] = &["", "low", "medium", "high", "xhigh", "max"];
const COLOR_OPTIONS: &[&str] = &["auto", "truecolor", "ansi"];
const IMAGE_OPTIONS: &[&str] = &["auto", "off"];
const PERM_OPTIONS: &[&str] = &["build", "auto", "plan", "bypass"];
const FONT_WEIGHT_OPTIONS: &[&str] = &["", "normal", "bold"];

#[rustfmt::skip]
pub static SETTINGS: &[SettingEntry] = &[
    // provider
    SettingEntry { dotted_key: "provider.default", label: "Default Route", description: "Default provider/model route (e.g. anthropic/claude-3-5-sonnet)", section: "provider", default_value: "", kind: SettingKind::String },
    SettingEntry { dotted_key: "provider.effort", label: "Reasoning Effort", description: "Base reasoning effort level", section: "provider", default_value: "medium", kind: SettingKind::Enum { options: EFFORT_OPTIONS } },
    SettingEntry { dotted_key: "provider.stream_idle_secs", label: "Stream Idle Timeout", description: "Base idle timeout in seconds for streaming responses", section: "provider", default_value: "180", kind: SettingKind::Integer { min: 1, max: 3600 } },
    SettingEntry { dotted_key: "provider.max_retries", label: "Max Retries", description: "Maximum retries for failed provider requests", section: "provider", default_value: "5", kind: SettingKind::Integer { min: 0, max: 100 } },

    // ui
    SettingEntry { dotted_key: "ui.hints.model", label: "Model Hint", description: "Show model route hint in prompt area", section: "ui", default_value: "false", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.hints.effort", label: "Effort Hint", description: "Show reasoning effort hint in prompt area", section: "ui", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.ascii_only", label: "ASCII Only", description: "Render plain ASCII box drawing and icons", section: "ui", default_value: "false", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.mouse", label: "Mouse Support", description: "Enable terminal mouse tracking and scrolling", section: "ui", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.auto_copy", label: "Auto Copy", description: "Automatically copy mouse selections to clipboard", section: "ui", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.ups", label: "Updates Per Second", description: "Render update rate: auto, dynamic, or fixed 1..=240", section: "ui", default_value: "10", kind: SettingKind::String },
    SettingEntry { dotted_key: "ui.color", label: "Color Mode", description: "Terminal color fidelity: auto, truecolor, or ansi", section: "ui", default_value: "auto", kind: SettingKind::Enum { options: COLOR_OPTIONS } },
    SettingEntry { dotted_key: "ui.transcript_cells", label: "Transcript Cells", description: "Max transcript cells kept in RAM before evicting", section: "ui", default_value: "500", kind: SettingKind::Integer { min: 10, max: 100000 } },
    SettingEntry { dotted_key: "ui.images", label: "Image Rendering", description: "Terminal image protocol: auto or off", section: "ui", default_value: "auto", kind: SettingKind::Enum { options: IMAGE_OPTIONS } },
    SettingEntry { dotted_key: "ui.vim", label: "Vim Keybindings", description: "Vim modal editing in prompt", section: "ui", default_value: "false", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "ui.thinking", label: "Show Thinking", description: "Draw model reasoning / thinking lines in transcript", section: "ui", default_value: "false", kind: SettingKind::Bool },

    // exec
    SettingEntry { dotted_key: "exec.yield_after_secs", label: "Yield After Seconds", description: "Foreground wait in seconds before backgrounding command", section: "exec", default_value: "10", kind: SettingKind::Integer { min: 1, max: 3600 } },
    SettingEntry { dotted_key: "exec.stall_secs", label: "Stall Seconds", description: "Silence duration in seconds before stall notification", section: "exec", default_value: "120", kind: SettingKind::Integer { min: 1, max: 86400 } },
    SettingEntry { dotted_key: "exec.max_spool_bytes", label: "Max Spool Bytes", description: "Maximum spool file size in bytes per task", section: "exec", default_value: "52428800", kind: SettingKind::Integer { min: 1024, max: 10737418240 } },
    SettingEntry { dotted_key: "exec.tail_bytes", label: "Tail Bytes", description: "In-memory tail buffer size in bytes per task", section: "exec", default_value: "65536", kind: SettingKind::Integer { min: 1024, max: 104857600 } },
    SettingEntry { dotted_key: "exec.kill_grace_secs", label: "Kill Grace Seconds", description: "Grace period in seconds between SIGTERM and SIGKILL", section: "exec", default_value: "5", kind: SettingKind::Integer { min: 1, max: 300 } },
    SettingEntry { dotted_key: "exec.max_tasks", label: "Max Tasks", description: "Maximum concurrent background tasks", section: "exec", default_value: "64", kind: SettingKind::Integer { min: 1, max: 1024 } },
    SettingEntry { dotted_key: "exec.default_timeout_secs", label: "Default Timeout Seconds", description: "Hard timeout in seconds for foreground command", section: "exec", default_value: "3600", kind: SettingKind::Integer { min: 1, max: 604800 } },

    // agents
    SettingEntry { dotted_key: "agents.max_live", label: "Max Live Agents", description: "Maximum concurrent live subagents", section: "agents", default_value: "8", kind: SettingKind::Integer { min: 1, max: 64 } },
    SettingEntry { dotted_key: "agents.default_model", label: "Subagent Model", description: "provider/model for subagents when unspecified", section: "agents", default_value: "", kind: SettingKind::String },
    SettingEntry { dotted_key: "agents.default_effort", label: "Subagent Effort", description: "Reasoning effort for subagents: low, medium, high, xhigh, max", section: "agents", default_value: "", kind: SettingKind::Enum { options: SUBAGENT_EFFORT_OPTIONS } },
    SettingEntry { dotted_key: "agents.max_turns", label: "Max Turns", description: "Maximum tool-calling turns for a subagent before stopping", section: "agents", default_value: "200", kind: SettingKind::Integer { min: 1, max: 10000 } },

    // daemon
    SettingEntry { dotted_key: "daemon.idle_timeout_secs", label: "Idle Timeout Seconds", description: "Seconds of inactivity before daemon shuts down (0 to disable)", section: "daemon", default_value: "600", kind: SettingKind::Integer { min: 0, max: 2592000 } },
    SettingEntry { dotted_key: "daemon.socket", label: "Socket Path", description: "Custom unix domain socket path for daemon IPC", section: "daemon", default_value: "", kind: SettingKind::String },

    // context
    SettingEntry { dotted_key: "context.compaction_threshold", label: "Compaction Threshold", description: "Fraction of context window that triggers compaction", section: "context", default_value: "0.85", kind: SettingKind::Float { min: 0.1, max: 1.0 } },
    SettingEntry { dotted_key: "context.tool_output_cap_chars", label: "Tool Output Cap Chars", description: "Maximum characters preserved from single tool output", section: "context", default_value: "16000", kind: SettingKind::Integer { min: 100, max: 10000000 } },
    SettingEntry { dotted_key: "context.injection_cap_chars", label: "Injection Cap Chars", description: "Maximum characters for system prompt injection files", section: "context", default_value: "4000", kind: SettingKind::Integer { min: 100, max: 10000000 } },
    SettingEntry { dotted_key: "context.instructions_cap_chars", label: "Instructions Cap Chars", description: "Cap for AGENTS.md / CLAUDE.md in system prompt", section: "context", default_value: "32000", kind: SettingKind::Integer { min: 100, max: 10000000 } },
    SettingEntry { dotted_key: "context.memory_cap_chars", label: "Memory Cap Chars", description: "Maximum characters for session memory", section: "context", default_value: "8000", kind: SettingKind::Integer { min: 100, max: 10000000 } },
    SettingEntry { dotted_key: "context.compaction_model", label: "Compaction Model", description: "Model to perform context compaction (default: session model)", section: "context", default_value: "", kind: SettingKind::String },
    SettingEntry { dotted_key: "context.keep_recent_messages", label: "Keep Recent Messages", description: "Recent messages kept verbatim after compaction", section: "context", default_value: "6", kind: SettingKind::Integer { min: 1, max: 100 } },
    SettingEntry { dotted_key: "context.default_context_window", label: "Default Context Window", description: "Fallback context window tokens when unknown", section: "context", default_value: "128000", kind: SettingKind::Integer { min: 1000, max: 10000000 } },

    // permissions
    SettingEntry { dotted_key: "permissions.default_mode", label: "Default Permission Mode", description: "Default mode: build, auto, plan, or bypass", section: "permissions", default_value: "build", kind: SettingKind::Enum { options: PERM_OPTIONS } },
    SettingEntry { dotted_key: "permissions.allow_catastrophic", label: "Allow Catastrophic", description: "Allow destructive commands without prompt in bypass mode", section: "permissions", default_value: "false", kind: SettingKind::Bool },

    // mcp
    SettingEntry { dotted_key: "mcp.servers", label: "MCP Servers", description: "Configured MCP servers (manage with /mcp or /import)", section: "mcp", default_value: "/mcp", kind: SettingKind::Action { command: "/mcp" } },
    SettingEntry { dotted_key: "mcp.sampling", label: "MCP Sampling", description: "Enable MCP client sampling capabilities", section: "mcp", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "mcp.sampling_max_tokens", label: "MCP Sampling Max Tokens", description: "Maximum tokens allowed per MCP sampling request", section: "mcp", default_value: "2048", kind: SettingKind::Integer { min: 1, max: 1000000 } },

    // session
    SettingEntry { dotted_key: "session.title_model", label: "Session Title Model", description: "Model used to name sessions after first reply", section: "session", default_value: "", kind: SettingKind::String },
    SettingEntry { dotted_key: "session.history_page", label: "History Page Size", description: "Transcript items per historical page load", section: "session", default_value: "200", kind: SettingKind::Integer { min: 1, max: 5000 } },

    // plugins
    SettingEntry { dotted_key: "plugins.enabled", label: "Plugins Enabled", description: "Enable plugin subsystem", section: "plugins", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "plugins.wasm_memory_mb", label: "WASM Memory (MB)", description: "Maximum RAM in MB allocated per WASM plugin", section: "plugins", default_value: "64", kind: SettingKind::Integer { min: 1, max: 4096 } },
    SettingEntry { dotted_key: "plugins.lua_memory_mb", label: "Lua Memory (MB)", description: "Maximum RAM in MB allocated per Lua plugin", section: "plugins", default_value: "32", kind: SettingKind::Integer { min: 1, max: 4096 } },
    SettingEntry { dotted_key: "plugins.hook_timeout_ms", label: "Hook Timeout (ms)", description: "Maximum milliseconds a plugin hook is allowed to run", section: "plugins", default_value: "5000", kind: SettingKind::Integer { min: 10, max: 60000 } },

    // skills
    SettingEntry { dotted_key: "skills.enabled", label: "Skills Enabled", description: "Enable skills discovery and invocation", section: "skills", default_value: "true", kind: SettingKind::Bool },
    SettingEntry { dotted_key: "skills.max_body_bytes", label: "Max Skill Body Bytes", description: "Maximum bytes per skill definition loaded", section: "skills", default_value: "16384", kind: SettingKind::Integer { min: 100, max: 10000000 } },
    SettingEntry { dotted_key: "skills.max_listed", label: "Max Skills Listed", description: "Maximum skills presented in tool catalogue", section: "skills", default_value: "100", kind: SettingKind::Integer { min: 1, max: 1000 } },

    // theme
    SettingEntry { dotted_key: "theme.name", label: "Theme Name", description: "Active color theme name (use /theme to pick or create)", section: "theme", default_value: "pacode-dark", kind: SettingKind::String },
    SettingEntry { dotted_key: "theme.overrides", label: "Theme Overrides", description: "Per-role color overrides (customize with /theme)", section: "theme", default_value: "/theme", kind: SettingKind::Action { command: "/theme" } },

    // font
    SettingEntry { dotted_key: "font.family", label: "Font Family", description: "Terminal font family request (best effort)", section: "font", default_value: "", kind: SettingKind::String },
    SettingEntry { dotted_key: "font.size", label: "Font Size", description: "Terminal font size in points (best effort)", section: "font", default_value: "", kind: SettingKind::Integer { min: 4, max: 72 } },
    SettingEntry { dotted_key: "font.weight", label: "Font Weight", description: "Terminal font weight: normal or bold", section: "font", default_value: "", kind: SettingKind::Enum { options: FONT_WEIGHT_OPTIONS } },

    // keys
    SettingEntry { dotted_key: "keys", label: "Keybindings", description: "Action keybindings (customize with /keys)", section: "keys", default_value: "/keys", kind: SettingKind::Action { command: "/keys" } },

    // web
    SettingEntry { dotted_key: "web.default_num_results", label: "Default Search Results", description: "Default number of search results to return", section: "web", default_value: "8", kind: SettingKind::Integer { min: 1, max: 50 } },
    SettingEntry { dotted_key: "web.request_timeout_secs", label: "Web Request Timeout", description: "Request timeout in seconds for web tools", section: "web", default_value: "15", kind: SettingKind::Integer { min: 1, max: 300 } },
];

pub fn all_settings() -> &'static [SettingEntry] {
    SETTINGS
}

pub fn find_entry(dotted_key: &str) -> Option<&'static SettingEntry> {
    SETTINGS.iter().find(|e| e.dotted_key == dotted_key)
}

pub fn read_value(config: &Config, dotted_key: &str) -> Option<String> {
    match dotted_key {
        "provider.default" => Some(config.provider.default.clone().unwrap_or_default()),
        "provider.effort" => Some(config.provider.effort.as_str().to_string()),
        "provider.stream_idle_secs" => Some(config.provider.stream_idle_secs.to_string()),
        "provider.max_retries" => Some(config.provider.max_retries.to_string()),
        "ui.hints.model" => Some(config.ui.hints.model.to_string()),
        "ui.hints.effort" => Some(config.ui.hints.effort.to_string()),
        "ui.ascii_only" => Some(config.ui.ascii_only.to_string()),
        "ui.mouse" => Some(config.ui.mouse.to_string()),
        "ui.auto_copy" => Some(config.ui.auto_copy.to_string()),
        "ui.ups" => Some(match config.ui.ups {
            Ups::Fixed(n) => n.to_string(),
            Ups::Dynamic => "dynamic".to_string(),
            Ups::Auto => "auto".to_string(),
        }),
        "ui.color" => Some(config.ui.color.clone()),
        "ui.transcript_cells" => Some(config.ui.transcript_cells.to_string()),
        "ui.images" => Some(config.ui.images.clone()),
        "ui.vim" => Some(config.ui.vim.to_string()),
        "ui.thinking" => Some(config.ui.thinking.to_string()),
        "exec.yield_after_secs" => Some(config.exec.yield_after_secs.to_string()),
        "exec.stall_secs" => Some(config.exec.stall_secs.to_string()),
        "exec.max_spool_bytes" => Some(config.exec.max_spool_bytes.to_string()),
        "exec.tail_bytes" => Some(config.exec.tail_bytes.to_string()),
        "exec.kill_grace_secs" => Some(config.exec.kill_grace_secs.to_string()),
        "exec.max_tasks" => Some(config.exec.max_tasks.to_string()),
        "exec.default_timeout_secs" => Some(config.exec.default_timeout_secs.to_string()),
        "agents.max_live" => Some(config.agents.max_live.to_string()),
        "agents.default_model" => Some(config.agents.default_model.clone().unwrap_or_default()),
        "agents.default_effort" => Some(
            config
                .agents
                .default_effort
                .map(|e| e.as_str().to_string())
                .unwrap_or_default(),
        ),
        "agents.max_turns" => Some(config.agents.max_turns.to_string()),
        "daemon.idle_timeout_secs" => Some(config.daemon.idle_timeout_secs.to_string()),
        "daemon.socket" => Some(
            config
                .daemon
                .socket
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        ),
        "context.compaction_threshold" => {
            Some(format!("{:.2}", config.context.compaction_threshold))
        }
        "context.tool_output_cap_chars" => Some(config.context.tool_output_cap_chars.to_string()),
        "context.injection_cap_chars" => Some(config.context.injection_cap_chars.to_string()),
        "context.instructions_cap_chars" => Some(config.context.instructions_cap_chars.to_string()),
        "context.memory_cap_chars" => Some(config.context.memory_cap_chars.to_string()),
        "context.compaction_model" => {
            Some(config.context.compaction_model.clone().unwrap_or_default())
        }
        "context.keep_recent_messages" => Some(config.context.keep_recent_messages.to_string()),
        "context.default_context_window" => Some(config.context.default_context_window.to_string()),
        "permissions.default_mode" => Some(config.permissions.default_mode.as_str().to_string()),
        "permissions.allow_catastrophic" => Some(config.permissions.allow_catastrophic.to_string()),
        "mcp.servers" => Some(format!("{} servers (/mcp)", config.mcp.servers.len())),
        "mcp.sampling" => Some(config.mcp.sampling.to_string()),
        "mcp.sampling_max_tokens" => Some(config.mcp.sampling_max_tokens.to_string()),
        "session.title_model" => Some(config.session.title_model.clone().unwrap_or_default()),
        "session.history_page" => Some(config.session.history_page.to_string()),
        "plugins.enabled" => Some(config.plugins.enabled.to_string()),
        "plugins.wasm_memory_mb" => Some(config.plugins.wasm_memory_mb.to_string()),
        "plugins.lua_memory_mb" => Some(config.plugins.lua_memory_mb.to_string()),
        "plugins.hook_timeout_ms" => Some(config.plugins.hook_timeout_ms.to_string()),
        "skills.enabled" => Some(config.skills.enabled.to_string()),
        "skills.max_body_bytes" => Some(config.skills.max_body_bytes.to_string()),
        "skills.max_listed" => Some(config.skills.max_listed.to_string()),
        "theme.name" => Some(config.theme.name.clone()),
        "theme.overrides" => Some(format!(
            "{} overrides (/theme)",
            config.theme.overrides.len()
        )),
        "font.family" => Some(config.font.family.clone().unwrap_or_default()),
        "font.size" => Some(config.font.size.map(|s| s.to_string()).unwrap_or_default()),
        "font.weight" => Some(config.font.weight.clone().unwrap_or_default()),
        "keys" => Some(format!(
            "{} custom bindings (/keys)",
            config.keys.bindings.len()
        )),
        "web.default_num_results" => Some(config.web.default_num_results.to_string()),
        "web.request_timeout_secs" => Some(config.web.request_timeout_secs.to_string()),
        _ => None,
    }
}

pub fn is_modified(config: &Config, entry: &SettingEntry) -> bool {
    read_value(config, entry.dotted_key).as_deref() != Some(entry.default_value)
}

pub fn validate_candidate(
    entry: &SettingEntry,
    candidate: &str,
) -> Result<toml::Value, ValidationError> {
    let trimmed = candidate.trim();
    match entry.kind {
        SettingKind::Bool => {
            if trimmed == "true" {
                Ok(toml::Value::Boolean(true))
            } else if trimmed == "false" {
                Ok(toml::Value::Boolean(false))
            } else {
                Err(ValidationError::InvalidBool {
                    value: candidate.to_string(),
                })
            }
        }
        SettingKind::Integer { min, max } => {
            if trimmed.is_empty() && entry.dotted_key == "font.size" {
                Ok(toml::Value::String(String::new()))
            } else {
                match trimmed.parse::<i64>() {
                    Ok(val) => {
                        if val < min || val > max {
                            Err(ValidationError::IntegerOutOfRange {
                                value: val,
                                min,
                                max,
                            })
                        } else {
                            Ok(toml::Value::Integer(val))
                        }
                    }
                    Err(source) => Err(ValidationError::InvalidInteger {
                        value: candidate.to_string(),
                        source,
                    }),
                }
            }
        }
        SettingKind::Float { min, max } => match trimmed.parse::<f64>() {
            Ok(val) => {
                if val.is_nan() || val < min || val > max {
                    Err(ValidationError::FloatOutOfRange {
                        value: val,
                        min,
                        max,
                    })
                } else {
                    Ok(toml::Value::Float(val))
                }
            }
            Err(source) => Err(ValidationError::InvalidFloat {
                value: candidate.to_string(),
                source,
            }),
        },
        SettingKind::Enum { options } => {
            if options.contains(&trimmed) {
                Ok(toml::Value::String(trimmed.to_string()))
            } else {
                Err(ValidationError::InvalidOption {
                    value: candidate.to_string(),
                    options: options.iter().map(|s| s.to_string()).collect(),
                })
            }
        }
        SettingKind::String => {
            if entry.dotted_key == "ui.ups" {
                if trimmed == "auto" || trimmed == "dynamic" {
                    Ok(toml::Value::String(trimmed.to_string()))
                } else if let Ok(n) = trimmed.parse::<i64>() {
                    if (1..=240).contains(&n) {
                        Ok(toml::Value::Integer(n))
                    } else {
                        Err(ValidationError::InvalidUps {
                            value: candidate.to_string(),
                        })
                    }
                } else {
                    Err(ValidationError::InvalidUps {
                        value: candidate.to_string(),
                    })
                }
            } else {
                Ok(toml::Value::String(trimmed.to_string()))
            }
        }
        SettingKind::Action { command } => Err(ValidationError::NotDirectlyEditable {
            key: entry.dotted_key.to_string(),
            command: command.to_string(),
        }),
    }
}

pub fn validate(dotted_key: &str, candidate: &str) -> Result<toml::Value, ValidationError> {
    let entry = find_entry(dotted_key).ok_or_else(|| ValidationError::UnknownKey {
        key: dotted_key.to_string(),
    })?;
    validate_candidate(entry, candidate)
}

pub fn apply_to_config(config: &mut Config, dotted_key: &str, value: &toml::Value) {
    match dotted_key {
        "provider.default" => {
            if let toml::Value::String(s) = value {
                config.provider.default = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "provider.effort" => {
            if let toml::Value::String(s) = value
                && let Some(eff) = Effort::parse(s)
            {
                config.provider.effort = eff;
            }
        }
        "provider.stream_idle_secs" => {
            if let toml::Value::Integer(n) = value {
                config.provider.stream_idle_secs = *n as u64;
            }
        }
        "provider.max_retries" => {
            if let toml::Value::Integer(n) = value {
                config.provider.max_retries = *n as u32;
            }
        }
        "ui.hints.model" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.hints.model = *b;
            }
        }
        "ui.hints.effort" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.hints.effort = *b;
            }
        }
        "ui.ascii_only" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.ascii_only = *b;
            }
        }
        "ui.mouse" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.mouse = *b;
            }
        }
        "ui.auto_copy" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.auto_copy = *b;
            }
        }
        "ui.ups" => match value {
            toml::Value::Integer(n) => config.ui.ups = Ups::Fixed(*n as u16),
            toml::Value::String(s) if s == "dynamic" => config.ui.ups = Ups::Dynamic,
            toml::Value::String(s) if s == "auto" => config.ui.ups = Ups::Auto,
            _ => {}
        },
        "ui.color" => {
            if let toml::Value::String(s) = value {
                config.ui.color = s.clone();
            }
        }
        "ui.transcript_cells" => {
            if let toml::Value::Integer(n) = value {
                config.ui.transcript_cells = *n as usize;
            }
        }
        "ui.images" => {
            if let toml::Value::String(s) = value {
                config.ui.images = s.clone();
            }
        }
        "ui.vim" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.vim = *b;
            }
        }
        "ui.thinking" => {
            if let toml::Value::Boolean(b) = value {
                config.ui.thinking = *b;
            }
        }
        "exec.yield_after_secs" => {
            if let toml::Value::Integer(n) = value {
                config.exec.yield_after_secs = *n as u64;
            }
        }
        "exec.stall_secs" => {
            if let toml::Value::Integer(n) = value {
                config.exec.stall_secs = *n as u64;
            }
        }
        "exec.max_spool_bytes" => {
            if let toml::Value::Integer(n) = value {
                config.exec.max_spool_bytes = *n as u64;
            }
        }
        "exec.tail_bytes" => {
            if let toml::Value::Integer(n) = value {
                config.exec.tail_bytes = *n as usize;
            }
        }
        "exec.kill_grace_secs" => {
            if let toml::Value::Integer(n) = value {
                config.exec.kill_grace_secs = *n as u64;
            }
        }
        "exec.max_tasks" => {
            if let toml::Value::Integer(n) = value {
                config.exec.max_tasks = *n as usize;
            }
        }
        "exec.default_timeout_secs" => {
            if let toml::Value::Integer(n) = value {
                config.exec.default_timeout_secs = *n as u64;
            }
        }
        "agents.max_live" => {
            if let toml::Value::Integer(n) = value {
                config.agents.max_live = *n as usize;
            }
        }
        "agents.default_model" => {
            if let toml::Value::String(s) = value {
                config.agents.default_model = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "agents.default_effort" => {
            if let toml::Value::String(s) = value {
                config.agents.default_effort = Effort::parse(s);
            }
        }
        "agents.max_turns" => {
            if let toml::Value::Integer(n) = value {
                config.agents.max_turns = *n as u32;
            }
        }
        "daemon.idle_timeout_secs" => {
            if let toml::Value::Integer(n) = value {
                config.daemon.idle_timeout_secs = *n as u64;
            }
        }
        "daemon.socket" => {
            if let toml::Value::String(s) = value {
                config.daemon.socket = if s.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(s))
                };
            }
        }
        "context.compaction_threshold" => {
            if let toml::Value::Float(f) = value {
                config.context.compaction_threshold = *f as f32;
            }
        }
        "context.tool_output_cap_chars" => {
            if let toml::Value::Integer(n) = value {
                config.context.tool_output_cap_chars = *n as usize;
            }
        }
        "context.injection_cap_chars" => {
            if let toml::Value::Integer(n) = value {
                config.context.injection_cap_chars = *n as usize;
            }
        }
        "context.instructions_cap_chars" => {
            if let toml::Value::Integer(n) = value {
                config.context.instructions_cap_chars = *n as usize;
            }
        }
        "context.memory_cap_chars" => {
            if let toml::Value::Integer(n) = value {
                config.context.memory_cap_chars = *n as usize;
            }
        }
        "context.compaction_model" => {
            if let toml::Value::String(s) = value {
                config.context.compaction_model = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "context.keep_recent_messages" => {
            if let toml::Value::Integer(n) = value {
                config.context.keep_recent_messages = *n as usize;
            }
        }
        "context.default_context_window" => {
            if let toml::Value::Integer(n) = value {
                config.context.default_context_window = *n as u32;
            }
        }
        "permissions.default_mode" => {
            if let toml::Value::String(s) = value
                && let Some(m) = Mode::parse(s)
            {
                config.permissions.default_mode = m;
            }
        }
        "permissions.allow_catastrophic" => {
            if let toml::Value::Boolean(b) = value {
                config.permissions.allow_catastrophic = *b;
            }
        }
        "mcp.sampling" => {
            if let toml::Value::Boolean(b) = value {
                config.mcp.sampling = *b;
            }
        }
        "mcp.sampling_max_tokens" => {
            if let toml::Value::Integer(n) = value {
                config.mcp.sampling_max_tokens = *n as u32;
            }
        }
        "session.title_model" => {
            if let toml::Value::String(s) = value {
                config.session.title_model = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "session.history_page" => {
            if let toml::Value::Integer(n) = value {
                config.session.history_page = *n as u32;
            }
        }
        "plugins.enabled" => {
            if let toml::Value::Boolean(b) = value {
                config.plugins.enabled = *b;
            }
        }
        "plugins.wasm_memory_mb" => {
            if let toml::Value::Integer(n) = value {
                config.plugins.wasm_memory_mb = *n as u32;
            }
        }
        "plugins.lua_memory_mb" => {
            if let toml::Value::Integer(n) = value {
                config.plugins.lua_memory_mb = *n as u32;
            }
        }
        "plugins.hook_timeout_ms" => {
            if let toml::Value::Integer(n) = value {
                config.plugins.hook_timeout_ms = *n as u64;
            }
        }
        "skills.enabled" => {
            if let toml::Value::Boolean(b) = value {
                config.skills.enabled = *b;
            }
        }
        "skills.max_body_bytes" => {
            if let toml::Value::Integer(n) = value {
                config.skills.max_body_bytes = *n as usize;
            }
        }
        "skills.max_listed" => {
            if let toml::Value::Integer(n) = value {
                config.skills.max_listed = *n as usize;
            }
        }
        "theme.name" => {
            if let toml::Value::String(s) = value {
                config.theme.name = s.clone();
            }
        }
        "font.family" => {
            if let toml::Value::String(s) = value {
                config.font.family = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "font.size" => match value {
            toml::Value::Integer(n) => config.font.size = Some(*n as u16),
            toml::Value::String(s) if s.is_empty() => config.font.size = None,
            _ => {}
        },
        "font.weight" => {
            if let toml::Value::String(s) = value {
                config.font.weight = if s.is_empty() { None } else { Some(s.clone()) };
            }
        }
        "web.default_num_results" => {
            if let toml::Value::Integer(n) = value {
                config.web.default_num_results = *n as usize;
            }
        }
        "web.request_timeout_secs" => {
            if let toml::Value::Integer(n) = value {
                config.web.request_timeout_secs = *n as u64;
            }
        }
        _ => {}
    }
}

pub fn reset_in_config(config: &mut Config, dotted_key: &str) {
    if let Some(entry) = find_entry(dotted_key)
        && let Ok(val) = validate_candidate(entry, entry.default_value)
    {
        apply_to_config(config, dotted_key, &val);
        return;
    }
    apply_to_config(config, dotted_key, &toml::Value::String(String::new()));
}
