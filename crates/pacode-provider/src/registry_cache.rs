//! In-memory credential cache for store-backed providers (`/login`, OAuth).
//!
//! * Owner: one [`TokenCache`] per `ProviderRegistry` (shared by its clones).
//! * Cap: one entry per provider id, so at most the number of configured providers;
//!   per-provider refresh locks are bounded the same way.
//! * Invalidation: `remove` on login, logout and `rebuild_provider_*`; an entry is also
//!   ignored once it is within [`REFRESH_MARGIN_SECS`] of its expiry, which sends the
//!   caller down the single-flight refresh path.
//!
//! Nothing here touches the disk. The caller persists a credential only when a refresh
//! actually changed it, so a rotated refresh token is written once and never dropped.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// Refresh when the token expires within this many seconds (same margin as `pacode-auth`).
pub(crate) const REFRESH_MARGIN_SECS: i64 = 60;

#[derive(Clone, Debug)]
struct Entry {
    token: String,
    /// Unix seconds or milliseconds, as stored in `auth.json`. `None` = never expires.
    expires_at: Option<i64>,
}

fn is_fresh(expires_at: Option<i64>, now_secs: i64) -> bool {
    match expires_at {
        None => true,
        Some(exp) => {
            let exp_secs = if exp > 100_000_000_000 {
                exp / 1000
            } else {
                exp
            };
            exp_secs > now_secs.saturating_add(REFRESH_MARGIN_SECS)
        }
    }
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// `PACODE_AUTH_LEGACY_RESOLVE=1` restores the per-step read/save behaviour.
pub(crate) fn legacy_resolve() -> bool {
    std::env::var("PACODE_AUTH_LEGACY_RESOLVE").is_ok_and(|v| v == "1")
}

#[derive(Default)]
pub(crate) struct TokenCache {
    entries: Mutex<HashMap<String, Entry>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl TokenCache {
    /// Token for `provider` if it is outside the refresh window.
    pub(crate) fn get_fresh(&self, provider: &str, now_secs: i64) -> Option<String> {
        let guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .get(provider)
            .filter(|e| is_fresh(e.expires_at, now_secs))
            .map(|e| e.token.clone())
    }

    pub(crate) fn put(&self, provider: &str, token: &str, expires_at: Option<i64>) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                provider.to_string(),
                Entry {
                    token: token.to_string(),
                    expires_at,
                },
            );
    }

    /// Recorded expiry of `provider`'s entry, if any.
    pub(crate) fn expiry(&self, provider: &str) -> Option<i64> {
        let guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        guard.get(provider).and_then(|e| e.expires_at)
    }

    pub(crate) fn remove(&self, provider: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(provider);
    }

    fn lock_for(&self, provider: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut guard = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(guard.entry(provider.to_string()).or_default())
    }

    /// Cached token, or one single-flight `refresh` per provider. Concurrent callers
    /// inside the window wait on the per-provider lock and then hit the cache the winner
    /// filled, so N callers cause one refresh.
    pub(crate) async fn ensure_fresh<F, Fut>(
        &self,
        provider: &str,
        refresh: F,
    ) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, Option<i64>), String>>,
    {
        if let Some(token) = self.get_fresh(provider, now_secs()) {
            return Ok(token);
        }
        let lock = self.lock_for(provider);
        let _flight = lock.lock().await;
        if let Some(token) = self.get_fresh(provider, now_secs()) {
            return Ok(token);
        }
        let (token, expires_at) = refresh().await?;
        self.put(provider, &token, expires_at);
        Ok(token)
    }
}

#[cfg(test)]
#[path = "registry_cache_tests.rs"]
mod registry_cache_tests;
