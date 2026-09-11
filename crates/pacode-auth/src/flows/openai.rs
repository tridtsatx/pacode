//! OAuth login and token refresh flow for OpenAI (ChatGPT/Codex).

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{AuthError, Result};
use crate::flows::{Progress, RefreshedTokens, urlencode};
use crate::pkce::{Pkce, random_state};
use crate::store::Account;

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_PATH: &str = "/auth/callback";
pub const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

const TIMEOUT: Duration = Duration::from_secs(120);

/// Return default redirect URI for OpenAI (`http://localhost:1455/auth/callback`).
pub fn default_redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}")
}

/// Build the OpenAI authorization URL for the browser.
pub fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    build_authorize_url_with_prompt(redirect_uri, challenge, state, None)
}

/// Build the OpenAI authorization URL with an optional prompt parameter.
pub fn build_authorize_url_with_prompt(
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    prompt: Option<&str>,
) -> String {
    let prompt_param = prompt
        .map(|p| format!("&prompt={}", urlencode(p)))
        .unwrap_or_default();
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=codex_cli_rs{prompt_param}",
        urlencode(redirect_uri),
        urlencode(SCOPES),
    )
}

/// Decode the payload portion of a JWT without cryptographic signature verification.
pub fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload_b64 = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| URL_SAFE.decode(payload_b64))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// Extract `chatgpt_account_id` from OpenAI `id_token` claims.
pub fn extract_account_id(id_token: &str) -> Option<String> {
    let payload = decode_jwt_payload(id_token)?;
    let auth = payload.get("https://api.openai.com/auth")?;
    auth.get("chatgpt_account_id")?
        .as_str()
        .map(|value| value.to_string())
}

/// Extract `email` claim from OpenAI `id_token`.
pub fn extract_email(id_token: &str) -> Option<String> {
    let payload = decode_jwt_payload(id_token)?;
    payload
        .get("email")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

/// Extract access token expiry (epoch seconds) from the access token JWT `exp` claim.
pub fn expires_at_from_access_token(access_token: &str) -> Option<i64> {
    let payload = decode_jwt_payload(access_token)?;
    payload.get("exp")?.as_i64()
}

/// Parse callback input containing code and state.
pub fn parse_callback_input_with_state(input: &str) -> Result<(String, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AuthError::Callback("empty callback input".into()));
    }

    let url = url::Url::parse(trimmed)
        .or_else(|_| url::Url::parse(&format!("http://localhost?{trimmed}")))
        .map_err(|e| AuthError::Callback(format!("failed to parse callback input: {e}")))?;

    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| AuthError::Callback("no code parameter found in callback URL".into()))?;

    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| AuthError::Callback("no state parameter found in callback URL".into()))?;

    Ok((code, state))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    id_token: Option<String>,
}

/// Exchange authorization code for OpenAI tokens against a specific endpoint URL.
pub async fn exchange_code_at_url(
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Account> {
    let body = format!(
        "grant_type=authorization_code&client_id={CLIENT_ID}&code={}&code_verifier={}&redirect_uri={}",
        urlencode(code),
        urlencode(verifier),
        urlencode(redirect_uri),
    );

    let client = crate::flows::auth_client_for_provider("openai")?;
    let resp = client
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(AuthError::Http)?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AuthError::Denied(format!(
            "token exchange failed (HTTP {status}): {text}"
        )));
    }

    let tokens: TokenResponse = resp.json().await.map_err(AuthError::Http)?;
    parse_openai_tokens("openai-1", tokens)
}

/// Exchange authorization code for OpenAI tokens using default endpoint.
pub async fn exchange_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<Account> {
    exchange_code_at_url(TOKEN_URL, code, verifier, redirect_uri).await
}

/// Exchange manual callback input (URL or query string) after validating state.
pub async fn exchange_callback_url(
    verifier: &str,
    input: &str,
    expected_state: &str,
    redirect_uri: &str,
) -> Result<crate::flows::LoginOutcome> {
    let (code, state) = parse_callback_input_with_state(input)?;
    if state != expected_state {
        return Err(AuthError::Callback(format!(
            "OAuth state mismatch: expected '{expected_state}', got '{state}'"
        )));
    }
    let account = exchange_code(&code, verifier, redirect_uri).await?;
    Ok(crate::flows::LoginOutcome {
        provider: "openai".to_string(),
        account,
    })
}

fn parse_openai_tokens(label: &str, tokens: TokenResponse) -> Result<Account> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // take expires_at from access token exp, fallback to expires_in
    let expires_at = expires_at_from_access_token(&tokens.access_token)
        .or_else(|| tokens.expires_in.map(|exp_in| now_secs + exp_in));

    let email = tokens.id_token.as_deref().and_then(extract_email);
    let account_id = tokens.id_token.as_deref().and_then(extract_account_id);

    let mut extra = serde_json::Map::new();
    if let Some(acc_id) = account_id {
        extra.insert("account_id".to_string(), serde_json::Value::String(acc_id));
    }
    if let Some(id_tok) = tokens.id_token {
        extra.insert("id_token".to_string(), serde_json::Value::String(id_tok));
    }

    Ok(Account {
        label: label.to_string(),
        kind: "oauth".to_string(),
        access: tokens.access_token,
        refresh: tokens.refresh_token,
        expires_at,
        email,
        extra,
    })
}

/// Refresh OpenAI tokens against a specific endpoint URL.
pub(crate) async fn refresh_tokens_at_url(
    token_url: &str,
    refresh_token: &str,
) -> Result<RefreshedTokens> {
    let body = format!(
        "grant_type=refresh_token&client_id={CLIENT_ID}&refresh_token={}",
        urlencode(refresh_token),
    );

    let client = crate::flows::auth_client_for_provider("openai")?;
    let resp = client
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(AuthError::Http)?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AuthError::Refresh(format!(
            "OpenAI token refresh failed (HTTP {status}): {text}"
        )));
    }

    let tokens: TokenResponse = resp.json().await.map_err(AuthError::Http)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let expires_at = expires_at_from_access_token(&tokens.access_token)
        .or_else(|| tokens.expires_in.map(|exp_in| now_secs + exp_in));

    let email = tokens.id_token.as_deref().and_then(extract_email);
    let account_id = tokens.id_token.as_deref().and_then(extract_account_id);

    Ok(RefreshedTokens {
        access: tokens.access_token,
        refresh: tokens
            .refresh_token
            .or_else(|| Some(refresh_token.to_string())),
        expires_at,
        email,
        account_id,
    })
}

/// Refresh OpenAI tokens using default or custom endpoint.
pub(crate) async fn refresh_tokens(
    refresh_token: &str,
    endpoint_override: Option<&str>,
) -> Result<RefreshedTokens> {
    let url = endpoint_override.unwrap_or(TOKEN_URL);
    refresh_tokens_at_url(url, refresh_token).await
}

/// Run browser login for OpenAI.
pub async fn login(
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<crate::flows::LoginOutcome> {
    let pkce = Pkce::generate();
    let state = random_state();
    let redirect_uri = default_redirect_uri();

    let listener_result = crate::callback::bind_callback(Some(CALLBACK_PORT));
    let listener = match listener_result {
        Ok(l) if l.port() == CALLBACK_PORT => l,
        _ => {
            // Port 1455 is busy; build auth URL so caller can display it and fall back to manual paste
            let auth_url = build_authorize_url_with_prompt(
                &redirect_uri,
                &pkce.challenge,
                &state,
                Some("login"),
            );
            let opened = crate::browser::open_url(&auth_url).is_ok();
            on_progress(Progress::OpenUrl {
                url: auth_url,
                opened,
            });
            return Err(AuthError::Callback(format!(
                "port {CALLBACK_PORT} is busy; manual paste fallback required (state: {state})"
            )));
        }
    };

    let auth_url =
        build_authorize_url_with_prompt(&redirect_uri, &pkce.challenge, &state, Some("login"));
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
        provider: "openai".to_string(),
        account,
    })
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod openai_tests;
