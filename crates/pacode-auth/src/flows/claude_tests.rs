use super::*;
use std::collections::HashMap;

#[test]
fn test_claude_authorize_url_query_parameters() {
    let redirect_uri = "http://127.0.0.1:42123/callback";
    let challenge = "test_challenge_hash_123";
    let state = "test_state_xyz";

    let url_str = build_authorize_url(redirect_uri, challenge, state);
    let parsed = url::Url::parse(&url_str).expect("parse authorize url");

    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.host_str(), Some("claude.com"));
    assert_eq!(parsed.path(), "/cai/oauth/authorize");

    let params: HashMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    assert_eq!(params.get("code").map(String::as_str), Some("true"));
    assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(redirect_uri)
    );
    assert_eq!(params.get("scope").map(String::as_str), Some(SCOPES));
    assert_eq!(
        params.get("code_challenge").map(String::as_str),
        Some(challenge)
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(params.get("state").map(String::as_str), Some(state));

    // Verify urlencoding in the raw query string (redirect_uri must have escaped colons and slashes)
    let raw_query = parsed.query().expect("raw query string");
    assert!(raw_query.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A42123%2Fcallback"));
}

#[tokio::test]
async fn test_claude_token_exchange_request_body_shape() {
    let success_response = serde_json::json!({
        "access_token": "sk-ant-access-123",
        "refresh_token": "sk-ant-refresh-456",
        "expires_in": 3600,
        "email": "user@example.com",
        "subscription_type": "max"
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &success_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/v1/oauth/token");

    let account = exchange_code_at_url(
        &mock_url,
        "auth_code_999",
        "pkce_verifier_777",
        "http://127.0.0.1:1234/callback",
        "state_abc",
    )
    .await
    .expect("exchange code at url");

    assert_eq!(account.access, "sk-ant-access-123");
    assert_eq!(account.refresh.as_deref(), Some("sk-ant-refresh-456"));
    assert_eq!(account.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        account.extra.get("subscription").and_then(|v| v.as_str()),
        Some("max")
    );

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );

    let body: serde_json::Value = serde_json::from_str(&recorded.body).expect("json body");
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "auth_code_999");
    assert_eq!(body["redirect_uri"], "http://127.0.0.1:1234/callback");
    assert_eq!(body["client_id"], CLIENT_ID);
    assert_eq!(body["code_verifier"], "pkce_verifier_777");
    assert_eq!(body["state"], "state_abc");
}

#[tokio::test]
async fn test_claude_token_refresh_request_body_shape() {
    let refresh_response = serde_json::json!({
        "access_token": "sk-ant-access-new",
        "refresh_token": "sk-ant-refresh-new",
        "expires_in": 7200
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &refresh_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/v1/oauth/token");

    let outcome = refresh_tokens_at_url(&mock_url, "sk-ant-refresh-old")
        .await
        .expect("refresh tokens");

    assert_eq!(outcome.access, "sk-ant-access-new");
    assert_eq!(outcome.refresh.as_deref(), Some("sk-ant-refresh-new"));

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );

    let body: serde_json::Value = serde_json::from_str(&recorded.body).expect("json body");
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["refresh_token"], "sk-ant-refresh-old");
    assert_eq!(body["client_id"], CLIENT_ID);
    assert_eq!(body["scope"], REFRESH_SCOPES);
}

#[test]
fn test_claude_code_input_parsing() {
    // Plain code
    let (code, state) = parse_claude_code_input("simple_code_123").expect("plain code");
    assert_eq!(code, "simple_code_123");
    assert!(state.is_none());

    // URL with query
    let (code, state) = parse_claude_code_input(
        "https://platform.claude.com/oauth/code/callback?code=query_code&state=query_state",
    )
    .expect("url code");
    assert_eq!(code, "query_code");
    assert_eq!(state.as_deref(), Some("query_state"));

    // Code with hash
    let (code, state) = parse_claude_code_input("hash_code#hash_state").expect("hash code");
    assert_eq!(code, "hash_code");
    assert_eq!(state.as_deref(), Some("hash_state"));
}

#[test]
fn test_claude_redirect_uri_for_input() {
    let loopback = "http://127.0.0.1:12345/callback";
    assert_eq!(
        claude_redirect_uri_for_input("simple_code", loopback),
        loopback
    );
    assert_eq!(
        claude_redirect_uri_for_input(
            "https://platform.claude.com/oauth/code/callback?code=xyz",
            loopback
        ),
        MANUAL_REDIRECT_URI
    );
    assert_eq!(
        claude_redirect_uri_for_input(
            "https://console.anthropic.com/oauth/code/callback?code=xyz",
            loopback
        ),
        MANUAL_REDIRECT_URI
    );
}
