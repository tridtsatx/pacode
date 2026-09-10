//! OAuth login flow for Devin (devin.ai / Cognition).

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{AuthError, Result};
use crate::flows::{Progress, urlencode};
use crate::pkce::{Pkce, random_state};
use crate::store::Account;

pub const CONNECT_RPC_URL: &str = "https://api.devin.ai/exa.seat_management_pb.SeatManagementService/ExchangePKCEAuthorizationCode";
pub const FALLBACK_URL: &str = "https://app.devin.ai/auth/cli/token";
pub const AUTHORIZE_URL: &str = "https://app.devin.ai/auth/cli/continue";

const TIMEOUT: Duration = Duration::from_secs(120);

/// Build the Devin authorization URL for the browser.
pub fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?redirect_uri={}&state={state}&prompt=select_account&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(redirect_uri),
    )
}

// NOTE: Request field names could not be confirmed from the official devin CLI binary
// (only response field names could). Pending verification against captured traffic.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevinExchangeRequest<'a> {
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevinExchangeResponse {
    pub api_key: String,
    #[serde(default)]
    pub api_server_url: Option<String>,
    #[serde(default)]
    pub devin_webapp_host: Option<String>,
    #[serde(default)]
    pub devin_api_url: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
}

/// Convert a successful Devin token exchange response into an [`Account`].
pub fn response_to_account(label: &str, resp: DevinExchangeResponse) -> Account {
    let mut extra = serde_json::Map::new();
    if let Some(url) = resp.api_server_url {
        extra.insert("api_server_url".to_string(), serde_json::Value::String(url));
    }
    if let Some(host) = resp.devin_webapp_host {
        extra.insert(
            "devin_webapp_host".to_string(),
            serde_json::Value::String(host),
        );
    }
    if let Some(url) = resp.devin_api_url {
        extra.insert("devin_api_url".to_string(), serde_json::Value::String(url));
    }
    if let Some(tok) = resp.session_token {
        extra.insert("session_token".to_string(), serde_json::Value::String(tok));
    }

    Account {
        label: label.to_string(),
        kind: "oauth".to_string(),
        access: resp.api_key,
        refresh: None,
        expires_at: None,
        email: None,
        extra,
    }
}

/// Exchange Devin authorization code against specified RPC and fallback URLs.
pub async fn exchange_code_at_urls(
    connect_url: &str,
    fallback_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Account> {
    let payload = DevinExchangeRequest {
        code,
        code_verifier: verifier,
        redirect_uri,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(connect_url)
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1")
        .json(&payload)
        .send()
        .await
        .map_err(AuthError::Http)?;

    let status = resp.status();
    let response_data = if status == reqwest::StatusCode::NOT_FOUND
        || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
        || status == reqwest::StatusCode::NOT_IMPLEMENTED
    {
        // Connect-RPC endpoint unavailable; fall back to POST /auth/cli/token
        let fallback_resp = client
            .post(fallback_url)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(AuthError::Http)?;

        let fb_status = fallback_resp.status();
        if !fb_status.is_success() {
            let body = fallback_resp.text().await.unwrap_or_default();
            return Err(AuthError::Denied(format!(
                "Devin fallback token exchange failed (HTTP {fb_status}): {body}"
            )));
        }

        fallback_resp
            .json::<DevinExchangeResponse>()
            .await
            .map_err(AuthError::Http)?
    } else if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Denied(format!(
            "Devin Connect-RPC exchange failed (HTTP {status}): {body}"
        )));
    } else {
        resp.json::<DevinExchangeResponse>()
            .await
            .map_err(AuthError::Http)?
    };

    Ok(response_to_account("devin-1", response_data))
}

/// Exchange authorization code for Devin tokens using standard endpoints.
pub async fn exchange_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<Account> {
    exchange_code_at_urls(CONNECT_RPC_URL, FALLBACK_URL, code, verifier, redirect_uri).await
}

/// Run browser login for Devin.
pub async fn login(
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<crate::flows::LoginOutcome> {
    // The official CLI already keeps a working credential on this machine, and its
    // token exchange is the one part of the flow we could not confirm against live
    // traffic. Reuse the existing credential when it is there: it is read in place,
    // never modified, and it makes the login instant and exact.
    if let Some(account) = crate::import::devin_account("devin-1")? {
        on_progress(Progress::Exchanging);
        return Ok(crate::flows::LoginOutcome {
            provider: "devin".to_string(),
            account,
        });
    }

    let pkce = Pkce::generate();
    let state = random_state();
    let listener = crate::callback::bind_callback(None)?;
    let redirect_uri = listener.redirect_uri("/callback");

    let auth_url = build_authorize_url(&redirect_uri, &pkce.challenge, &state);
    let opened = crate::browser::open_url(&auth_url).is_ok();
    on_progress(Progress::OpenUrl {
        url: auth_url,
        opened,
    });

    on_progress(Progress::Waiting);
    let params = listener.wait(TIMEOUT, &state).await?;
    let code = params.into_result()?;

    on_progress(Progress::Exchanging);
    let account = exchange_code(&code, &pkce.verifier, &redirect_uri).await?;

    Ok(crate::flows::LoginOutcome {
        provider: "devin".to_string(),
        account,
    })
}

#[cfg(test)]
#[path = "devin_tests.rs"]
mod devin_tests;
