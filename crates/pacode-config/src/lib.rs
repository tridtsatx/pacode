//! Paths, config loading and the file logger.
//!
//! Layout (XDG, all overridable by `PACODE_HOME` which then holds everything):
//! - config:  `~/.config/pacode/config.toml`   (`PACODE_CONFIG` overrides the file),
//!   plus `providers.toml` (`[providers]`) and `keybindings.toml` (`[keys]`)
//!   next to it — merged on load, later files win. A legacy `config.toml`
//!   carrying `[providers]`/`[keys]` is migrated once and kept as `config.toml.bak`.
//! - data:    `~/.local/share/pacode/`  → `pacode.db`
//! - state:   `~/.local/state/pacode/`  → `daemon.log`, `tasks/<id>.out`, `tool-output/`
//! - cache:   `~/.cache/pacode/`        → `mcp/<server>.json`
//! - runtime: `$XDG_RUNTIME_DIR/pacode/` → `daemon.sock` (fallback `/tmp/pacode-<uid>/`)

pub mod display;
pub mod logging;
pub mod paths;
pub mod prefs;
pub mod registry;
pub mod theme;
pub mod watcher;

pub use display::detect_refresh_hz;
pub use paths::Paths;
pub use prefs::{Prefs, load_prefs, save_prefs};
pub use theme::{load_theme, user_theme_names, write_theme};
pub use toml;
pub use watcher::ConfigWatcher;

use pacode_types::{Config, Effort, Mode, ProviderConfig};
use serde::Deserialize;

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

/// Sibling config filename for the `[providers]` table.
pub const PROVIDERS_FILENAME: &str = "providers.toml";
/// Sibling config filename for the `[keys]` table.
pub const KEYBINDINGS_FILENAME: &str = "keybindings.toml";

/// `providers.toml` / `keybindings.toml` live next to `config.toml`.
fn sibling_file(config_path: &std::path::Path, name: &str) -> std::path::PathBuf {
    config_path
        .parent()
        .map(|p| p.join(name))
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

/// Read a TOML file into a table; `Ok(None)` when the file is missing.
fn read_table(path: &std::path::Path) -> Result<Option<toml::Table>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str::<toml::Table>(&text)
                .map(Some)
                .map_err(|e| ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source: err,
        }),
    }
}

fn write_table(path: &std::path::Path, table: &toml::Table) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let serialized = toml::to_string_pretty(table).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    std::fs::write(path, serialized).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Deep-merge `overlay` into `base`: tables merge recursively, anything else
/// (and type mismatches) is overwritten by the overlay value.
fn deep_merge(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_t)), toml::Value::Table(over_t)) => {
                deep_merge(base_t, over_t);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

const DEPRECATED_AGENT_KEYS: &[&str] = &["default_model", "default_effort"];

/// Keys the runtime never read, grouped by the table they lived in. serde drops
/// them silently, which is not enough: a user who set one still believes the
/// cap is in force, so each is worth a line in the log.
const DEAD_LIMITS: &[(&str, &[&str])] = &[
    (
        "limits",
        &[
            "max_logical_agents",
            "max_provider_queue_items",
            "max_provider_queue_bytes",
            "max_exec_calls_per_turn",
            "max_tool_calls_per_turn",
        ],
    ),
    ("agents", &["max_live", "max_turns"]),
];

fn migrate_deprecated_agent_keys(table: &mut toml::Table) {
    let Some(agents) = table.get_mut("agents").and_then(toml::Value::as_table_mut) else {
        return;
    };
    let removed = DEPRECATED_AGENT_KEYS
        .iter()
        .copied()
        .filter(|key| agents.remove(*key).is_some())
        .collect::<Vec<_>>();
    if !removed.is_empty() {
        log::warn!(
            "deprecated [agents] keys ignored: {}; subagents inherit the parent model and effort",
            removed.join(", ")
        );
    }
}

fn warn_about_dead_limits(table: &toml::Table) {
    let present = DEAD_LIMITS
        .iter()
        .flat_map(|(section, keys)| {
            let section_table = table.get(*section).and_then(toml::Value::as_table);
            keys.iter()
                .filter(move |key| {
                    section_table.is_some_and(|table| table.contains_key::<str>(*key))
                })
                .map(move |key| format!("{section}.{key}"))
        })
        .collect::<Vec<_>>();
    if present.is_empty() {
        return;
    }
    log::warn!(
        "no-op settings ignored: {}; pacode caps no agents, no provider queue and no per-turn calls",
        present.join(", ")
    );
}

/// A value out of the registry's range is repaired to the nearest working one
/// and reported, never refused: a config written against an older pacode, or
/// against bounds that have since moved, must still boot.
fn deserialize_config(mut table: toml::Table) -> Result<Config, String> {
    migrate_deprecated_agent_keys(&mut table);
    warn_about_dead_limits(&table);
    let mut config =
        Config::deserialize(toml::Value::Table(table)).map_err(|error| error.to_string())?;
    for repair in registry::repair_config(&mut config) {
        log::warn!(
            "config {} out of range ({}); using a working value until the file is edited",
            repair.key,
            repair.message
        );
    }
    Ok(config)
}

/// One-time migration: a legacy `config.toml` carrying `[providers]` and/or
/// `[keys]` tables is split into `providers.toml`/`keybindings.toml` next to it.
/// The original is preserved as `config.toml.bak`; values already present in the
/// new files win on conflicts, legacy keys are merged in. Idempotent: once the
/// sections are gone from `config.toml` this is a no-op.
fn migrate_split_files(config_path: &std::path::Path) -> Result<(), ConfigError> {
    let Some(mut table) = read_table(config_path)? else {
        return Ok(());
    };

    let has_providers = table.contains_key("providers");
    let has_keys = table.contains_key("keys");
    if !has_providers && !has_keys {
        return Ok(());
    }

    // Back up the original first so a crash mid-migration loses nothing; an
    // existing .bak is kept (it is the oldest untouched original).
    let backup = sibling_file(config_path, "config.toml.bak");
    if !backup.exists() {
        std::fs::copy(config_path, &backup).map_err(|e| ConfigError::Write {
            path: backup.clone(),
            source: e,
        })?;
    }

    for (section, filename) in [
        ("providers", PROVIDERS_FILENAME),
        ("keys", KEYBINDINGS_FILENAME),
    ] {
        let Some(legacy) = table.remove(section) else {
            continue;
        };
        let target = sibling_file(config_path, filename);
        let mut target_table = read_table(&target)?.unwrap_or_default();
        // Merge: existing file's section wins over the legacy one.
        let merged = match (legacy, target_table.remove(section)) {
            (toml::Value::Table(mut legacy_t), Some(toml::Value::Table(existing_t))) => {
                deep_merge(&mut legacy_t, existing_t);
                toml::Value::Table(legacy_t)
            }
            (_, Some(existing)) => existing,
            (legacy, None) => legacy,
        };
        target_table.insert(section.to_string(), merged);
        write_table(&target, &target_table)?;
    }

    write_table(config_path, &table)?;
    // Marker the TUI consumes on next attach to toast the migration once
    // (spec §5); failing to write it only means no notice is shown.
    let _ = std::fs::write(
        sibling_file(config_path, "config.toml.migrated"),
        MIGRATION_NOTICE,
    );
    log::info!(
        "migrated [providers]/[keys] out of {}",
        config_path.display()
    );
    Ok(())
}

/// Human message delivered once via toast on the next attach after a
/// config-split migration (spec §5).
const MIGRATION_NOTICE: &str = "migrated [providers]/[keys] to providers.toml/keybindings.toml; original kept as config.toml.bak";

/// One-shot notice for the config-split migration: when `load_from_path`
/// migrated a legacy `config.toml` it left a `config.toml.migrated` marker
/// next to it; the first caller to observe the marker gets the message and
/// the marker is removed, so later attaches don't repeat the toast.
pub fn take_migration_notice(paths: &Paths) -> Option<String> {
    let marker = paths
        .config_file
        .parent()
        .map(|p| p.join("config.toml.migrated"))?;
    if !marker.is_file() {
        return None;
    }
    let msg = std::fs::read_to_string(&marker)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| MIGRATION_NOTICE.to_string());
    let _ = std::fs::remove_file(&marker);
    Some(msg)
}

/// Load config from `path` plus its `providers.toml` / `keybindings.toml`
/// siblings, run legacy-section migration, and apply environment overrides.
/// Later files win on conflicting keys; missing files are treated as empty.
pub fn load_from_path(path: &std::path::Path) -> Result<Config, ConfigError> {
    migrate_split_files(path)?;

    let mut merged = read_table(path)?.unwrap_or_default();
    for file in [
        sibling_file(path, PROVIDERS_FILENAME),
        sibling_file(path, KEYBINDINGS_FILENAME),
    ] {
        if let Some(table) = read_table(&file)? {
            deep_merge(&mut merged, table);
        }
    }

    let mut cfg = deserialize_config(merged).map_err(|message| ConfigError::Parse {
        path: path.to_path_buf(),
        message,
    })?;

    let env_vars = std::env::vars().collect::<Vec<_>>();
    apply_env_overrides(
        &mut cfg,
        env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )?;
    Ok(cfg)
}

/// Load the config files (missing files = defaults) and apply environment overrides:
/// `PACODE_MODEL` (`provider/model`), `PACODE_EFFORT`, `PACODE_MODE`.
pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
    load_from_path(&paths.config_file)
}

/// Parse TOML text into a `Config` (used by `load` and by tests).
pub fn parse(text: &str) -> Result<Config, String> {
    let table = toml::from_str::<toml::Table>(text).map_err(|error| error.to_string())?;
    deserialize_config(table)
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

/// Which config file owns a dotted key: `providers.*` → `providers.toml`,
/// `keys.*` → `keybindings.toml`, everything else → `config.toml`.
pub fn file_for_key(paths: &Paths, dotted_key: &str) -> std::path::PathBuf {
    match dotted_key.split('.').next() {
        Some("providers") => paths.providers_file(),
        Some("keys") => paths.keybindings_file(),
        _ => paths.config_file.clone(),
    }
}

/// Set a nested key in the owning split file (`providers.*` → `providers.toml`,
/// `keys.*` → `keybindings.toml`, else `config.toml`) by parsing the existing
/// file as a `toml::Table`, creating tables for intermediate keys as needed,
/// setting `value` at the target key, and writing back. Comments are lost.
pub fn update_config_value(
    paths: &Paths,
    dotted_key: &str,
    value: toml::Value,
) -> Result<(), ConfigError> {
    if dotted_key.is_empty() {
        return Ok(());
    }
    let file = file_for_key(paths, dotted_key);
    let mut table = read_table(&file)?.unwrap_or_default();

    let parts: Vec<&str> = dotted_key.split('.').collect();
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

    write_table(&file, &table)
}

/// Remove a nested key in the owning split file (same routing as
/// [`update_config_value`]). If the file, key or intermediate tables do not
/// exist, this is a no-op. Comments are lost.
pub fn remove_config_value(paths: &Paths, dotted_key: &str) -> Result<(), ConfigError> {
    if dotted_key.is_empty() {
        return Ok(());
    }
    let file = file_for_key(paths, dotted_key);
    let Some(mut table) = read_table(&file)? else {
        return Ok(());
    };

    let parts: Vec<&str> = dotted_key.split('.').collect();
    let (parents, last) = parts.split_at(parts.len() - 1);
    let last_key = last[0];

    let mut current = &mut table;
    for &part in parents {
        match current.get_mut(part) {
            Some(toml::Value::Table(t)) => current = t,
            _ => return Ok(()),
        }
    }
    current.remove(last_key);

    write_table(&file, &table)
}

#[cfg(test)]
mod config_tests;
