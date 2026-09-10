//! Authentication seam, credential status inspection, and background login coordinator.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_auth::store::{Account, AuthStore};
use pacode_types::protocol::{AuthState, ProviderAuthInfo};
use pacode_types::{ProviderConfig, now_ms};

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;

/// Stage updates yielded by an in-flight login flow.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    OpenUrl { url: String, opened: bool },
    Waiting,
    Exchanging,
}

/// The successful outcome of a login flow.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginOutcome {
    pub provider: String,
    pub account: Account,
}

pub type ProgressCallback = Arc<dyn Fn(Progress) + Send + Sync>;

/// Abstraction seam for executing login flows and retrieving access tokens.
#[async_trait::async_trait]
pub trait AuthDriver: Send + Sync {
    async fn login(
        &self,
        provider: &str,
        on_progress: ProgressCallback,
    ) -> Result<LoginOutcome, String>;

    async fn access_token(&self, provider: &str) -> Result<String, String>;
}

/// Default driver used in live daemon runtime.
#[derive(Clone, Default)]
pub struct DefaultAuthDriver;

#[async_trait::async_trait]
impl AuthDriver for DefaultAuthDriver {
    async fn login(
        &self,
        provider: &str,
        on_progress: ProgressCallback,
    ) -> Result<LoginOutcome, String> {
        let forward: Arc<dyn Fn(pacode_auth::flows::Progress) + Send + Sync> =
            Arc::new(move |stage: pacode_auth::flows::Progress| {
                on_progress(match stage {
                    pacode_auth::flows::Progress::OpenUrl { url, opened } => {
                        Progress::OpenUrl { url, opened }
                    }
                    pacode_auth::flows::Progress::Waiting => Progress::Waiting,
                    pacode_auth::flows::Progress::Exchanging => Progress::Exchanging,
                });
            });
        let outcome = pacode_auth::flows::login(provider, forward)
            .await
            .map_err(|e| e.to_string())?;
        Ok(LoginOutcome {
            provider: outcome.provider,
            account: outcome.account,
        })
    }

    async fn access_token(&self, provider: &str) -> Result<String, String> {
        pacode_auth::flows::access_token(provider)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Build the provider authentication status vector from `LOGIN_PROVIDERS` joined with `AuthStore`.
pub fn build_auth_status(
    store: &AuthStore,
    config_providers: &BTreeMap<String, ProviderConfig>,
) -> Vec<ProviderAuthInfo> {
    let mut infos = Vec::new();

    for desc in pacode_auth::catalog::LOGIN_PROVIDERS {
        let id = desc.id.to_string();
        let display_name = desc.display_name.to_string();
        let auth_kind = match desc.auth_kind {
            pacode_auth::catalog::AuthKind::OAuth => "oauth".to_string(),
            pacode_auth::catalog::AuthKind::ApiKey => "api_key".to_string(),
            pacode_auth::catalog::AuthKind::Local => "local".to_string(),
        };
        let detail = desc.detail.to_string();
        let recommended = desc.recommended;

        // Check accounts in store for this provider ID or known aliases
        let mut accounts = store.accounts(desc.id).to_vec();
        let mut active = store.active_label(desc.id).map(String::from);
        if accounts.is_empty() {
            for alias in desc.aliases {
                let alias_accounts = store.accounts(alias);
                if !alias_accounts.is_empty() {
                    accounts = alias_accounts.to_vec();
                    active = store.active_label(alias).map(String::from);
                    break;
                }
            }
        }

        let account_labels: Vec<String> = accounts.iter().map(|a| a.label.clone()).collect();

        // Determine AuthState
        let state = if desc.auth_kind == pacode_auth::catalog::AuthKind::Local {
            AuthState::Configured
        } else if let Some(active_label) = &active {
            if let Some(active_acc) = accounts.iter().find(|a| &a.label == active_label) {
                if active_acc.access.trim().is_empty() {
                    AuthState::NeedsAttention {
                        reason: "empty credentials".to_string(),
                    }
                } else if let Some(exp) = active_acc.expires_at {
                    let now_secs = (now_ms() / 1000) as i64;
                    let exp_secs = if exp > 100_000_000_000 {
                        exp / 1000
                    } else {
                        exp
                    };
                    if exp_secs <= now_secs && active_acc.refresh.is_none() {
                        AuthState::NeedsAttention {
                            reason: "token expired and no refresh token available".to_string(),
                        }
                    } else {
                        AuthState::Configured
                    }
                } else {
                    AuthState::Configured
                }
            } else {
                AuthState::NeedsAttention {
                    reason: "active account not found in accounts list".to_string(),
                }
            }
        } else {
            // Check if config.providers has an explicit API key for this provider or aliases
            let has_config_key = config_providers
                .get(desc.id)
                .is_some_and(|p| p.api_key.is_some() || p.api_key_env.is_some())
                || desc.aliases.iter().any(|alias| {
                    config_providers
                        .get(*alias)
                        .is_some_and(|p| p.api_key.is_some() || p.api_key_env.is_some())
                });

            if has_config_key {
                AuthState::Configured
            } else {
                AuthState::NotConfigured
            }
        };

        infos.push(ProviderAuthInfo {
            id,
            display_name,
            auth_kind,
            detail,
            recommended,
            state,
            accounts: account_labels,
            active,
        });
    }

    infos
}

/// Retrieve the `ProviderAuthInfo` for a single provider ID.
pub fn get_provider_auth_info(
    store: &AuthStore,
    config_providers: &BTreeMap<String, ProviderConfig>,
    provider: &str,
) -> Option<ProviderAuthInfo> {
    let all = build_auth_status(store, config_providers);
    let matched = pacode_auth::catalog::find(provider)
        .map(|d| d.id)
        .unwrap_or(provider);
    all.into_iter()
        .find(|info| info.id == matched || info.id == provider)
}
