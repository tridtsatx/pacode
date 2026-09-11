//! Provider OAuth login flows and access token coordinator.

pub mod claude;
pub mod devin;
pub mod openai;

#[cfg(test)]
pub(crate) mod test_support;

use std::sync::{Arc, LazyLock};

use crate::error::{AuthError, Result};
use crate::refresh::{RefreshState, SingleFlight};
use crate::store::{Account, AuthStore};

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    OpenUrl { url: String, opened: bool },
    Waiting,
    Exchanging,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoginOutcome {
    pub provider: String,
    pub account: Account,
}

#[derive(Clone, Debug)]
pub(crate) struct RefreshedTokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Option<i64>,
    pub email: Option<String>,
    pub account_id: Option<String>,
}

static SINGLE_FLIGHT: LazyLock<SingleFlight<RefreshedTokens>> = LazyLock::new(SingleFlight::new);
static REFRESH_STATE: LazyLock<RefreshState> = LazyLock::new(RefreshState::new);

/// Build an HTTP client for an auth flow using the provider's configured proxy setting.
pub(crate) fn auth_client_for_provider(provider_id: &str) -> Result<reqwest::Client> {
    let paths = pacode_config::Paths::discover();
    let cfg = pacode_config::load(&paths).map_err(|e| AuthError::Config(e.to_string()))?;
    auth_client_from_config(&cfg, provider_id)
}

/// Build an HTTP client from an explicit configuration object.
pub(crate) fn auth_client_from_config(
    cfg: &pacode_types::Config,
    provider_id: &str,
) -> Result<reqwest::Client> {
    let provider_proxy = cfg
        .providers
        .get(provider_id)
        .and_then(|p| p.proxy.as_deref());
    let global_proxy = cfg.provider.proxy.as_deref();
    let setting = pacode_net::ProxySetting::resolve(provider_proxy, global_proxy).map_err(|e| {
        AuthError::Config(format!("provider '{provider_id}' has invalid proxy: {e}"))
    })?;
    pacode_net::client_builder(&setting)
        .map_err(|e| AuthError::Config(format!("provider '{provider_id}' proxy error: {e}")))?
        .build()
        .map_err(AuthError::Http)
}

/// Pure-Rust RFC 3986 percent-encoder.
pub(crate) fn urlencode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 3 / 2);
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{b:02X}");
            }
        }
    }
    encoded
}

/// Runs the whole browser login for `provider` (`claude` | `openai` | `devin`).
/// Reports each stage through `on_progress`. Does NOT touch the store.
pub async fn login(
    provider: &str,
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<LoginOutcome> {
    let canonical = provider.trim().to_ascii_lowercase();
    match canonical.as_str() {
        "claude" | "anthropic" => claude::login(on_progress).await,
        "openai" | "chatgpt" => openai::login(on_progress).await,
        "devin" | "cognition" => devin::login(on_progress).await,
        _ => Err(AuthError::UnknownProvider(format!(
            "unsupported login provider '{provider}'"
        ))),
    }
}

/// Returns a usable access token for the active account, refreshing it first when it
/// is expired or within 60s of expiry. Single-flight per provider+label. Persists a
/// refreshed token back into the store.
pub async fn access_token(provider: &str) -> Result<String> {
    let mut store = AuthStore::load()?;
    let token = access_token_in(&mut store, provider).await?;
    let _ = store.save();
    Ok(token)
}

/// Same, but against a caller-provided store (used by tests and by the daemon).
pub async fn access_token_in(store: &mut AuthStore, provider: &str) -> Result<String> {
    access_token_in_with_endpoint(store, provider, None).await
}

pub(crate) async fn access_token_in_with_endpoint(
    store: &mut AuthStore,
    provider: &str,
    endpoint_override: Option<&str>,
) -> Result<String> {
    let account = store.get(provider).ok_or_else(|| {
        AuthError::UnknownProvider(format!("no active account found for provider '{provider}'"))
    })?;

    // Devin or API key credentials do not require network refresh.
    if provider == "devin" || account.kind != "oauth" || account.refresh.is_none() {
        return Ok(account.access.clone());
    }

    let refresh_token = match &account.refresh {
        Some(rt) if !rt.trim().is_empty() => rt.clone(),
        _ => return Ok(account.access.clone()),
    };

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let needs_refresh = match account.expires_at {
        Some(exp) => {
            let exp_secs = if exp > 100_000_000_000 {
                exp / 1000
            } else {
                exp
            };
            exp_secs <= now_secs + 60
        }
        None => false,
    };

    if !needs_refresh {
        return Ok(account.access.clone());
    }

    let flight_key = format!("{provider}:{}", account.label);
    REFRESH_STATE.ensure_allowed(&flight_key)?;

    let provider_name = provider.to_string();
    let endpoint_override = endpoint_override.map(|s| s.to_string());
    let flight_key_for_closure = flight_key.clone();

    let outcome = SINGLE_FLIGHT
        .run(&flight_key, move || {
            let flight_key = flight_key_for_closure;
            let refresh_token = refresh_token.clone();
            let provider_name = provider_name.clone();
            async move {
                let res = match provider_name.as_str() {
                    "claude" | "anthropic" => {
                        claude::refresh_tokens(&refresh_token, endpoint_override.as_deref()).await
                    }
                    "openai" | "chatgpt" => {
                        openai::refresh_tokens(&refresh_token, endpoint_override.as_deref()).await
                    }
                    _ => Err(AuthError::Refresh(format!(
                        "refresh not supported for provider '{provider_name}'"
                    ))),
                };

                match &res {
                    Ok(_) => {
                        REFRESH_STATE.record_outcome(&flight_key, Ok(()));
                    }
                    Err(err) => {
                        let err_msg = format!("{err}");
                        if is_terminal_refresh_error(&err_msg) {
                            REFRESH_STATE.record_outcome(&flight_key, Err(&err_msg));
                        }
                    }
                }

                res
            }
        })
        .await?;

    let mut updated_account = store.get(provider).cloned().ok_or_else(|| {
        AuthError::UnknownProvider(format!("account vanished for provider '{provider}'"))
    })?;

    updated_account.access = outcome.access.clone();
    if let Some(new_rf) = outcome.refresh {
        updated_account.refresh = Some(new_rf);
    }
    if let Some(new_exp) = outcome.expires_at {
        updated_account.expires_at = Some(new_exp);
    }
    if let Some(em) = outcome.email {
        updated_account.email = Some(em.clone());
        updated_account
            .extra
            .insert("email".to_string(), serde_json::Value::String(em));
    }
    if let Some(acc_id) = outcome.account_id {
        updated_account
            .extra
            .insert("account_id".to_string(), serde_json::Value::String(acc_id));
    }

    store.upsert(provider, updated_account);
    let _ = store.save();

    Ok(outcome.access)
}

pub(crate) fn is_terminal_refresh_error(err_msg: &str) -> bool {
    let lower = err_msg.to_ascii_lowercase();
    lower.contains("invalid_grant")
        || lower.contains("unauthorized_client")
        || lower.contains("invalid_client")
        || lower.contains("unsupported_grant_type")
        || lower.contains("token has been revoked")
        || lower.contains("revoked")
}

#[cfg(test)]
pub(crate) fn clear_refresh_state_for_test(key: &str) {
    REFRESH_STATE.clear(key);
}

#[cfg(test)]
#[path = "flows_tests.rs"]
mod flows_tests;
