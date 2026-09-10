//! Reuse of credentials another CLI already stored on this machine.
//!
//! Files written by other tools are read in place and never modified, moved or
//! rewritten. Only the fields pacode needs are taken.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{AuthError, Result};
use crate::store::Account;

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;

/// `~/.local/share/devin/credentials.toml`, as written by the official `devin` CLI.
#[derive(Debug, Deserialize)]
struct DevinCredentialsFile {
    /// The account credential. Named after the Windsurf lineage of that CLI.
    windsurf_api_key: String,
    #[serde(default)]
    api_server_url: Option<String>,
    #[serde(default)]
    devin_webapp_host: Option<String>,
    #[serde(default)]
    devin_api_url: Option<String>,
}

/// Default location of the official Devin CLI credentials.
pub fn devin_credentials_path() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    match data {
        Some(dir) => dir.join("devin/credentials.toml"),
        None => PathBuf::from("devin/credentials.toml"),
    }
}

/// Read the Devin CLI credentials if they exist. `Ok(None)` when the file is absent,
/// an error when it exists but cannot be used — a half-usable credential is worse
/// than a clear failure.
pub fn devin_account(label: &str) -> Result<Option<Account>> {
    devin_account_from(devin_credentials_path(), label)
}

pub fn devin_account_from(path: impl AsRef<Path>, label: &str) -> Result<Option<Account>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(AuthError::Io)?;
    let parsed: DevinCredentialsFile = toml::from_str(&text).map_err(|e| {
        AuthError::Store(format!(
            "{} is not a readable devin credentials file: {e}",
            path.display()
        ))
    })?;
    if parsed.windsurf_api_key.trim().is_empty() {
        return Err(AuthError::Denied(format!(
            "{} holds no api key; run `devin auth login` first",
            path.display()
        )));
    }

    let mut extra = serde_json::Map::new();
    for (key, value) in [
        ("api_server_url", parsed.api_server_url),
        ("devin_webapp_host", parsed.devin_webapp_host),
        ("devin_api_url", parsed.devin_api_url),
    ] {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            extra.insert(key.to_string(), serde_json::Value::String(v));
        }
    }
    extra.insert(
        "imported_from".to_string(),
        serde_json::Value::String(path.display().to_string()),
    );
    // The official CLI mints a per-session token on top of the stored key; without a
    // captured login we reuse the key for both halves of the credential pair.
    let session = parsed.windsurf_api_key.clone();
    extra.insert(
        "session_token".to_string(),
        serde_json::Value::String(session),
    );

    Ok(Some(Account {
        label: label.to_string(),
        kind: "oauth".to_string(),
        access: parsed.windsurf_api_key,
        refresh: None,
        expires_at: None,
        email: None,
        extra,
    }))
}
