//! Paths, config loading and the file logger.
//!
//! Layout (XDG, all overridable by `CODEAPP_HOME` which then holds everything):
//! - config:  `~/.config/codeapp/config.toml`   (`CODEAPP_CONFIG` overrides the file)
//! - data:    `~/.local/share/codeapp/`  → `codeapp.db`
//! - state:   `~/.local/state/codeapp/`  → `daemon.log`, `tasks/<id>.out`, `tool-output/`
//! - cache:   `~/.cache/codeapp/`        → `mcp/<server>.json`
//! - runtime: `$XDG_RUNTIME_DIR/codeapp/` → `daemon.sock` (fallback `/tmp/codeapp-<uid>/`)

pub mod logging;
pub mod paths;

pub use paths::Paths;

use codeapp_types::{Config, Effort, Mode, ProviderConfig};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path}: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("invalid environment override {var}={value}")]
    Env { var: String, value: String },
}

/// Load the config file (missing file = defaults) and apply environment overrides:
/// `CODEAPP_MODEL` (`provider/model`), `CODEAPP_EFFORT`, `CODEAPP_MODE`.
pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
    let _ = paths;
    todo!("config::load")
}

/// Parse TOML text into a `Config` (used by `load` and by tests).
pub fn parse(text: &str) -> Result<Config, String> {
    let _ = text;
    todo!("config::parse")
}

/// Apply `CODEAPP_MODEL` / `CODEAPP_EFFORT` / `CODEAPP_MODE` from `vars` (an iterator of
/// `(name, value)` so tests do not touch the real environment).
pub fn apply_env_overrides<'a>(
    cfg: &mut Config,
    vars: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), ConfigError> {
    let _ = (cfg, vars);
    todo!("config::apply_env_overrides")
}

/// The API key for a provider: inline `api_key`, else the env var named by `api_key_env`.
pub fn resolve_api_key(cfg: &ProviderConfig) -> Option<String> {
    let _ = cfg;
    todo!("config::resolve_api_key")
}

/// Effective effort: CLI/env override, else `[provider].effort`.
pub fn effective_effort(cfg: &Config, override_effort: Option<Effort>) -> Effort {
    override_effort.unwrap_or(cfg.provider.effort)
}

/// Effective mode: CLI/env override, else `[permissions].default_mode`.
pub fn effective_mode(cfg: &Config, override_mode: Option<Mode>) -> Mode {
    override_mode.unwrap_or(cfg.permissions.default_mode)
}
