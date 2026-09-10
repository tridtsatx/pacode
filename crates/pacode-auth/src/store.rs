//! Persistent credential store for OAuth tokens and API keys.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{AuthError, Result};

/// Stored credential for a single account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Account label or identifier (e.g. `"claude-1"`).
    pub label: String,
    /// Credential kind (e.g. `"oauth"`, `"api_key"`).
    pub kind: String,
    /// Access token or secret key.
    pub access: String,
    /// Optional OAuth refresh token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    /// Token expiry timestamp in unix epoch seconds or milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Associated user email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Provider-specific metadata (e.g. Devin's API URLs, org_id).
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Provider entry holding multiple accounts and the active account label.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Currently active account label for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// List of configured accounts for this provider.
    #[serde(default)]
    pub accounts: Vec<Account>,
}

/// Root data model for `auth.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthStoreData {
    /// Schema format version (default: 1).
    pub version: u32,
    /// Per-provider account collections.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
}

impl Default for AuthStoreData {
    fn default() -> Self {
        Self {
            version: 1,
            providers: BTreeMap::new(),
        }
    }
}

/// Atomic filesystem credential store.
#[derive(Clone, Debug)]
pub struct AuthStore {
    path: PathBuf,
    data: AuthStoreData,
}

impl AuthStore {
    /// Load the credential store from the default discovery location:
    /// `Paths::discover().auth_file()` (`~/.pacode/state/auth.json`).
    ///
    /// If the file does not exist, returns an empty store without error.
    /// If the file exists but is corrupted, returns [`AuthError::Store`].
    pub fn load() -> Result<Self> {
        let paths = pacode_config::Paths::discover();
        Self::load_from(paths.auth_file())
    }

    /// Load the credential store from a specific path.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                data: AuthStoreData::default(),
            });
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            AuthError::Store(format!(
                "failed to read auth file at {}: {e}",
                path.display()
            ))
        })?;

        let data: AuthStoreData = serde_json::from_str(&content).map_err(|e| {
            AuthError::Store(format!("corrupt auth file at {}: {e}", path.display()))
        })?;

        Ok(Self { path, data })
    }

    /// Create an empty in-memory store configured to save to `path`.
    pub fn new_empty(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            data: AuthStoreData::default(),
        }
    }

    /// Return the file path associated with this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Save the credential store atomically to its configured path.
    ///
    /// Creates parent directories with mode `0700` on Unix.
    /// Writes to a temporary file in the same directory, applies mode `0600` on Unix,
    /// and atomically renames the temporary file over the target path.
    pub fn save(&self) -> Result<()> {
        self.save_to(&self.path)
    }

    /// Save the credential store atomically to the specified path.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let dir = path.parent().unwrap_or_else(|| Path::new("."));

        // Ensure directory exists with 0700 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            builder.create(dir).map_err(AuthError::Io)?;

            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(dir).map_err(AuthError::Io)?;
        }

        let rand_suffix: u64 = rand::random();
        let file_stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("auth.json");
        let temp_path = dir.join(format!(".{file_stem}.{rand_suffix}.tmp"));

        let serialized = serde_json::to_string_pretty(&self.data)?;

        // Write content to temporary file
        std::fs::write(&temp_path, serialized.as_bytes()).map_err(AuthError::Io)?;

        // Restrict permissions to 0600 BEFORE rename
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(err) =
                std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
            {
                let _ = std::fs::remove_file(&temp_path);
                return Err(AuthError::Io(err));
            }
        }

        // Atomic rename
        if let Err(err) = std::fs::rename(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(AuthError::Io(err));
        }

        Ok(())
    }

    /// Return the active account for `provider`, if one is set.
    pub fn get(&self, provider: &str) -> Option<&Account> {
        let entry = self.data.providers.get(provider)?;
        let active_label = entry.active.as_deref()?;
        entry.accounts.iter().find(|acc| acc.label == active_label)
    }

    /// Return all configured accounts for `provider`.
    pub fn accounts(&self, provider: &str) -> &[Account] {
        self.data
            .providers
            .get(provider)
            .map(|e| e.accounts.as_slice())
            .unwrap_or(&[])
    }

    /// Return the active account label for `provider`.
    pub fn active_label(&self, provider: &str) -> Option<&str> {
        self.data
            .providers
            .get(provider)
            .and_then(|e| e.active.as_deref())
    }

    /// Insert or update an account for `provider`.
    ///
    /// If an account with the same label already exists, it is replaced in place.
    /// If no active account is currently set for `provider`, this account becomes active.
    pub fn upsert(&mut self, provider: &str, account: Account) {
        let entry = self.data.providers.entry(provider.to_string()).or_default();
        if entry.active.is_none() {
            entry.active = Some(account.label.clone());
        }

        if let Some(pos) = entry.accounts.iter().position(|a| a.label == account.label) {
            entry.accounts[pos] = account;
        } else {
            entry.accounts.push(account);
        }
    }

    /// Remove the account with `label` from `provider`.
    ///
    /// If the removed account was active, activates the first remaining account,
    /// or clears the active label if none remain. Returns `true` if an account was removed.
    pub fn remove(&mut self, provider: &str, label: &str) -> bool {
        let Some(entry) = self.data.providers.get_mut(provider) else {
            return false;
        };

        let initial_len = entry.accounts.len();
        entry.accounts.retain(|a| a.label != label);
        let removed = entry.accounts.len() < initial_len;

        if removed && entry.active.as_deref() == Some(label) {
            entry.active = entry.accounts.first().map(|a| a.label.clone());
        }

        removed
    }

    /// Set the active account label for `provider`.
    ///
    /// Fails with [`AuthError::Store`] if the account label is not found.
    pub fn set_active(&mut self, provider: &str, label: &str) -> Result<()> {
        let entry = self.data.providers.get_mut(provider).ok_or_else(|| {
            AuthError::UnknownProvider(format!("provider '{provider}' not found in store"))
        })?;

        if !entry.accounts.iter().any(|a| a.label == label) {
            return Err(AuthError::Store(format!(
                "account '{label}' not found for provider '{provider}'"
            )));
        }

        entry.active = Some(label.to_string());
        Ok(())
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
