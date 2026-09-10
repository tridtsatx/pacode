//! OAuth login and token refresh flow for Claude (Anthropic).

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{AuthError, Result};
use crate::flows::{Progress, RefreshedTokens, urlencode};
use crate::pkce::Pkce;
use crate::store::Account;

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const MANUAL_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
pub const LEGACY_MANUAL_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
pub const REFRESH_SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

const TIMEOUT: Duration = Duration::from_secs(120);

/// Build the Claude authorization URL for the browser.
pub fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={CLIENT_ID}&response_type=code&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        urlencode(redirect_uri),
        urlencode(SCOPES),
    )
}

/// Determine the redirect URI based on whether input came from a manual paste or the loopback server.
pub fn claude_redirect_uri_for_input(input: &str, fallback_redirect_uri: &str) -> String {
    let trimmed = input.trim();
    let Ok(url) = url::Url::parse(trimmed) else {
        return fallback_redirect_uri.to_string();
    };

    let matches_manual = [MANUAL_REDIRECT_URI, LEGACY_MANUAL_REDIRECT_URI]
        .iter()
        .filter_map(|candidate| url::Url::parse(candidate).ok())
        .any(|expected| {
            url.scheme() == expected.scheme()
                && url.host_str() == expected.host_str()
                && url.path() == expected.path()
        });

    if matches_manual {
        MANUAL_REDIRECT_URI.to_string()
    } else {
        fallback_redirect_uri.to_string()
    }
}

/// Parse Claude auth code input (plain code, URL with `code=`, or `code#state`).
pub fn parse_claude_code_input(input: &str) -> Result<(String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AuthError::Callback("no authorization code provided".into()));
    }

    let (raw_code, state_from_query) = if trimmed.contains("code=") {
        let url = url::Url::parse(trimmed)
            .or_else(|_| url::Url::parse(&format!("https://example.com?{trimmed}")))
            .map_err(|e| AuthError::Callback(format!("invalid callback URL: {e}")))?;

        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| AuthError::Callback("no code found in URL".into()))?;

        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());

        (code, state)
    } else {
        (trimmed.to_string(), None)
    };

    let (code, state) = if raw_code.contains('#') {
        let parts: Vec<&str> = raw_code.splitn(2, '#').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (raw_code, state_from_query)
    };

    if code.trim().is_empty() {
        return Err(AuthError::Callback("no authorization code provided".into()));
    }

    Ok((code, state))
}

#[derive(Serialize)]
struct ClaudeExchangeRequest<'a> {
    grant_type: &'static str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'static str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Serialize)]
struct ClaudeRefreshRequest<'a> {
    grant_type: &'static str,
    refresh_token: &'a str,
    client_id: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'static str>,
}

#[derive(Deserialize)]
struct ClaudeTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subscription: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
    #[serde(default)]
    account: Option<ClaudeTokenAccount>,
}

#[derive(Deserialize, Default)]
struct ClaudeTokenAccount {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
}

/// Exchange authorization code for Claude tokens against a specific token endpoint.
pub async fn exchange_code_at_url(
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<Account> {
    let payload = ClaudeExchangeRequest {
        grant_type: "authorization_code",
        code,
        redirect_uri,
        client_id: CLIENT_ID,
        code_verifier: verifier,
        state,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(token_url)
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(AuthError::Http)?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Denied(format!(
            "token exchange failed (HTTP {status}): {body}"
        )));
    }

    let token_resp: ClaudeTokenResponse = resp.json().await.map_err(AuthError::Http)?;
    parse_claude_token_response("claude-1", token_resp)
}

/// Exchange authorization code for Claude tokens using default endpoint.
pub async fn exchange_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<Account> {
    exchange_code_at_url(TOKEN_URL, code, verifier, redirect_uri, state).await
}

fn parse_claude_token_response(label: &str, resp: ClaudeTokenResponse) -> Result<Account> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let expires_at = Some(now_secs + resp.expires_in);

    let email = resp
        .email
        .or_else(|| resp.account.as_ref().and_then(|a| a.email.clone()));

    let subscription = resp.subscription.or(resp.subscription_type).or_else(|| {
        resp.account
            .as_ref()
            .and_then(|a| a.subscription_type.clone())
    });

    let mut extra = serde_json::Map::new();
    if let Some(ref em) = email {
        extra.insert("email".to_string(), serde_json::Value::String(em.clone()));
    }
    if let Some(sub) = subscription {
        extra.insert("subscription".to_string(), serde_json::Value::String(sub));
    }
    if let Some(sc) = resp.scope {
        extra.insert("scope".to_string(), serde_json::Value::String(sc));
    }

    Ok(Account {
        label: label.to_string(),
        kind: "oauth".to_string(),
        access: resp.access_token,
        refresh: resp.refresh_token,
        expires_at,
        email,
        extra,
    })
}

/// Refresh Claude tokens against a specific endpoint.
pub(crate) async fn refresh_tokens_at_url(
    token_url: &str,
    refresh_token: &str,
) -> Result<RefreshedTokens> {
    let client = reqwest::Client::new();

    // First attempt with REFRESH_SCOPES
    let payload = ClaudeRefreshRequest {
        grant_type: "refresh_token",
        refresh_token,
        client_id: CLIENT_ID,
        scope: Some(REFRESH_SCOPES),
    };

    let resp = client
        .post(token_url)
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(AuthError::Http)?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();

    let token_resp: ClaudeTokenResponse = if !status.is_success() {
        let is_invalid_scope = body_text.to_ascii_lowercase().contains("invalid_scope");
        if is_invalid_scope {
            // Retry without scope for compatibility
            let retry_payload = ClaudeRefreshRequest {
                grant_type: "refresh_token",
                refresh_token,
                client_id: CLIENT_ID,
                scope: None,
            };

            let retry_resp = client
                .post(token_url)
                .header("content-type", "application/json")
                .json(&retry_payload)
                .send()
                .await
                .map_err(AuthError::Http)?;

            let retry_status = retry_resp.status();
            if !retry_status.is_success() {
                let retry_body = retry_resp.text().await.unwrap_or_default();
                return Err(AuthError::Refresh(format!(
                    "token refresh failed (HTTP {retry_status}): {retry_body}"
                )));
            }

            retry_resp.json().await.map_err(AuthError::Http)?
        } else {
            return Err(AuthError::Refresh(format!(
                "token refresh failed (HTTP {status}): {body_text}"
            )));
        }
    } else {
        serde_json::from_str(&body_text)?
    };

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    Ok(RefreshedTokens {
        access: token_resp.access_token,
        refresh: token_resp
            .refresh_token
            .or_else(|| Some(refresh_token.to_string())),
        expires_at: Some(now_secs + token_resp.expires_in),
        email: token_resp.email,
        account_id: None,
    })
}

/// Refresh Claude tokens using default or custom endpoint.
pub(crate) async fn refresh_tokens(
    refresh_token: &str,
    endpoint_override: Option<&str>,
) -> Result<RefreshedTokens> {
    let url = endpoint_override.unwrap_or(TOKEN_URL);
    refresh_tokens_at_url(url, refresh_token).await
}

/// Run browser login for Claude.
pub async fn login(
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<crate::flows::LoginOutcome> {
    let pkce = Pkce::generate();
    let listener = crate::callback::bind_callback(None)?;
    let redirect_uri = listener.redirect_uri("/callback");
    // Anthropic OAuth checks that the state returned from the callback matches the verifier
    let state = pkce.verifier.clone();

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
    let account = exchange_code(&code, &pkce.verifier, &redirect_uri, &state).await?;

    Ok(crate::flows::LoginOutcome {
        provider: "claude".to_string(),
        account,
    })
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod claude_tests;
