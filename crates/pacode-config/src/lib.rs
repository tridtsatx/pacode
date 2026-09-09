//! Paths, config loading and the file logger.
//!
//! Layout (XDG, all overridable by `PACODE_HOME` which then holds everything):
//! - config:  `~/.config/pacode/config.toml`   (`PACODE_CONFIG` overrides the file)
//! - data:    `~/.local/share/pacode/`  → `pacode.db`
//! - state:   `~/.local/state/pacode/`  → `daemon.log`, `tasks/<id>.out`, `tool-output/`
//! - cache:   `~/.cache/pacode/`        → `mcp/<server>.json`
//! - runtime: `$XDG_RUNTIME_DIR/pacode/` → `daemon.sock` (fallback `/tmp/pacode-<uid>/`)

pub mod display;
pub mod logging;
pub mod paths;
pub mod prefs;
pub mod theme;

pub use display::detect_refresh_hz;
pub use paths::Paths;
pub use prefs::{Prefs, load_prefs, save_prefs};
pub use theme::{load_theme, user_theme_names, write_theme};
pub use toml;

use pacode_types::{Config, Effort, Mode, ProviderConfig};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
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
/// `PACODE_MODEL` (`provider/model`), `PACODE_EFFORT`, `PACODE_MODE`.
pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
    let mut cfg = match std::fs::read_to_string(&paths.config_file) {
        Ok(text) => parse(&text).map_err(|message| ConfigError::Parse {
            path: paths.config_file.clone(),
            message,
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => {
            return Err(ConfigError::Read {
                path: paths.config_file.clone(),
                source: err,
            });
        }
    };

    let env_vars = std::env::vars().collect::<Vec<_>>();
    apply_env_overrides(
        &mut cfg,
        env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )?;
    Ok(cfg)
}

/// Parse TOML text into a `Config` (used by `load` and by tests).
pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str::<Config>(text).map_err(|e| e.to_string())
}

/// Apply `PACODE_MODEL` / `PACODE_EFFORT` / `PACODE_MODE` from `vars` (an iterator of
/// `(name, value)` so tests do not touch the real environment).
pub fn apply_env_overrides<'a>(
    cfg: &mut Config,
    vars: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), ConfigError> {
    for (k, v) in vars {
        match k {
            "PACODE_MODEL" => {
                cfg.provider.default = Some(v.to_string());
            }
            "PACODE_EFFORT" => {
                let effort = Effort::parse(v).ok_or_else(|| ConfigError::Env {
                    var: k.to_string(),
                    value: v.to_string(),
                })?;
                cfg.provider.effort = effort;
            }
            "PACODE_MODE" => {
                let mode = Mode::parse(v).ok_or_else(|| ConfigError::Env {
                    var: k.to_string(),
                    value: v.to_string(),
                })?;
                cfg.permissions.default_mode = mode;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The API key for a provider: inline `api_key`, else the env var named by `api_key_env`.
pub fn resolve_api_key(cfg: &ProviderConfig) -> Option<String> {
    if let Some(key) = &cfg.api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(env_var) = &cfg.api_key_env {
        let trimmed_var = env_var.trim();
        if !trimmed_var.is_empty()
            && let Ok(val) = std::env::var(trimmed_var)
        {
            let trimmed_val = val.trim();
            if !trimmed_val.is_empty() {
                return Some(trimmed_val.to_string());
            }
        }
    }
    None
}

/// Effective effort: CLI/env override, else `[provider].effort`.
pub fn effective_effort(cfg: &Config, override_effort: Option<Effort>) -> Effort {
    override_effort.unwrap_or(cfg.provider.effort)
}

/// Effective mode: CLI/env override, else `[permissions].default_mode`.
pub fn effective_mode(cfg: &Config, override_mode: Option<Mode>) -> Mode {
    override_mode.unwrap_or(cfg.permissions.default_mode)
}

/// Set a nested key in `paths.config_file` by parsing the existing file as a `toml::Table`,
/// creating tables for intermediate keys as needed, setting `value` at the target key,
/// and writing back. Comments in the file are lost.
pub fn update_config_value(
    paths: &Paths,
    dotted_key: &str,
    value: toml::Value,
) -> Result<(), ConfigError> {
    let mut table: toml::Table = match std::fs::read_to_string(&paths.config_file) {
        Ok(text) => toml::from_str(&text).map_err(|e: toml::de::Error| ConfigError::Parse {
            path: paths.config_file.clone(),
            message: e.to_string(),
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(err) => {
            return Err(ConfigError::Read {
                path: paths.config_file.clone(),
                source: err,
            });
        }
    };

    let parts: Vec<&str> = dotted_key.split('.').collect();
    if parts.is_empty() || dotted_key.is_empty() {
        return Ok(());
    }

    let (parents, last) = parts.split_at(parts.len() - 1);
    let last_key = last[0];

    let mut current = &mut table;
    for &part in parents {
        let entry = current
            .entry(part)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        current = match entry {
            toml::Value::Table(t) => t,
            _ => unreachable!(),
        };
    }
    current.insert(last_key.to_string(), value);

    if let Some(parent) = paths.config_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let serialized = toml::to_string_pretty(&table).map_err(|e| ConfigError::Parse {
        path: paths.config_file.clone(),
        message: e.to_string(),
    })?;

    std::fs::write(&paths.config_file, serialized).map_err(|e| ConfigError::Write {
        path: paths.config_file.clone(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod config_tests;
