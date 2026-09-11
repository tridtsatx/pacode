use super::*;
use std::collections::HashMap;

#[test]
fn test_devin_authorize_url_query_parameters() {
    let redirect_uri = "http://127.0.0.1:54321/callback";
    let challenge = "devin_challenge_hash_abc";
    let state = "devin_state_xyz";

    let url_str = build_authorize_url(redirect_uri, challenge, state);
    let parsed = url::Url::parse(&url_str).expect("parse authorize url");

    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.host_str(), Some("app.devin.ai"));
    assert_eq!(parsed.path(), "/auth/cli/continue");

    let params: HashMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(redirect_uri)
    );
    assert_eq!(params.get("state").map(String::as_str), Some(state));
    assert_eq!(
        params.get("prompt").map(String::as_str),
        Some("select_account")
    );
    assert_eq!(
        params.get("code_challenge").map(String::as_str),
        Some(challenge)
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    // The official CLI marks its PKCE logins with this; captured 2026-09-11.
    assert_eq!(params.get("cli_pkce_marker").map(String::as_str), Some("1"));

    // Verify raw query urlencoding
    let raw = parsed.query().expect("raw query");
    assert!(raw.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A54321%2Fcallback"));
}

#[tokio::test]
async fn test_devin_token_exchange_connect_rpc_body_shape() {
    let success_body = serde_json::json!({
        "api_key": "cog-devin-api-key-test",
        "api_server_url": "https://api.devin.ai",
        "devin_webapp_host": "app.devin.ai",
        "devin_api_url": "https://api.devin.ai/v1",
        "session_token": "sess-xyz-123"
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &success_body).await;
    let mock_connect_url = format!(
        "http://127.0.0.1:{port}/exa.seat_management_pb.SeatManagementService/ExchangePKCEAuthorizationCode"
    );
    let mock_fallback_url = format!("http://127.0.0.1:{port}/auth/cli/token");

    let account = exchange_code_at_urls(
        &mock_connect_url,
        &mock_fallback_url,
        "devin_auth_code_1",
        "devin_verifier_1",
        "http://127.0.0.1:5000/callback",
    )
    .await
    .expect("exchange code at connect rpc");

    assert_eq!(account.access, "cog-devin-api-key-test");
    assert_eq!(account.refresh, None);
    assert_eq!(account.expires_at, None);
    assert_eq!(account.kind, "oauth");
    assert_eq!(
        account.extra.get("api_server_url").and_then(|v| v.as_str()),
        Some("https://api.devin.ai")
    );
    assert_eq!(
        account
            .extra
            .get("devin_webapp_host")
            .and_then(|v| v.as_str()),
        Some("app.devin.ai")
    );
    assert_eq!(
        account.extra.get("devin_api_url").and_then(|v| v.as_str()),
        Some("https://api.devin.ai/v1")
    );
    assert_eq!(
        account.extra.get("session_token").and_then(|v| v.as_str()),
        Some("sess-xyz-123")
    );

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        recorded
            .headers
            .get("connect-protocol-version")
            .map(String::as_str),
        Some("1")
    );

    let body: serde_json::Value = serde_json::from_str(&recorded.body).expect("json body");
    assert_eq!(body["code"], "devin_auth_code_1");
    assert_eq!(body["code_verifier"], "devin_verifier_1");
    assert_eq!(body["redirect_uri"], "http://127.0.0.1:5000/callback");
}

#[tokio::test]
async fn test_devin_token_exchange_fallback_on_404() {
    let fallback_body = serde_json::json!({
        "api_key": "fallback-api-key-success",
        "api_server_url": "https://fallback.devin.ai"
    })
    .to_string();

    let mut routes = HashMap::new();
    let rpc_path =
        "/exa.seat_management_pb.SeatManagementService/ExchangePKCEAuthorizationCode".to_string();
    let token_path = "/auth/cli/token".to_string();
    routes.insert(
        rpc_path.clone(),
        (404, r#"{"error":"not_found"}"#.to_string()),
    );
    routes.insert(token_path.clone(), (200, fallback_body));

    let (port, handle) = crate::flows::test_support::mock_server_multi_route(routes, 2).await;
    let mock_connect_url = format!("http://127.0.0.1:{port}{rpc_path}");
    let mock_fallback_url = format!("http://127.0.0.1:{port}{token_path}");

    let account = exchange_code_at_urls(
        &mock_connect_url,
        &mock_fallback_url,
        "devin_fallback_code",
        "devin_fallback_verifier",
        "http://127.0.0.1:5000/callback",
    )
    .await
    .expect("fallback exchange should succeed");

    assert_eq!(account.access, "fallback-api-key-success");
    assert_eq!(
        account.extra.get("api_server_url").and_then(|v| v.as_str()),
        Some("https://fallback.devin.ai")
    );

    let requests = handle.await.expect("join handle");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, rpc_path);
    assert_eq!(requests[1].path, token_path);
}

#[test]
fn test_devin_response_mapping_into_account_with_extra() {
    let resp = DevinExchangeResponse {
        api_key: "my_devin_key".to_string(),
        api_server_url: Some("https://api.devin.ai".to_string()),
        devin_webapp_host: Some("app.devin.ai".to_string()),
        devin_api_url: Some("https://api.devin.ai/v1".to_string()),
        session_token: Some("tok_12345".to_string()),
    };

    let account = response_to_account("devin-1", resp);
    assert_eq!(account.label, "devin-1");
    assert_eq!(account.kind, "oauth");
    assert_eq!(account.access, "my_devin_key");
    assert!(account.refresh.is_none());
    assert!(account.expires_at.is_none());
    assert!(account.email.is_none());

    assert_eq!(
        account.extra.get("api_server_url").and_then(|v| v.as_str()),
        Some("https://api.devin.ai")
    );
    assert_eq!(
        account
            .extra
            .get("devin_webapp_host")
            .and_then(|v| v.as_str()),
        Some("app.devin.ai")
    );
    assert_eq!(
        account.extra.get("devin_api_url").and_then(|v| v.as_str()),
        Some("https://api.devin.ai/v1")
    );
    assert_eq!(
        account.extra.get("session_token").and_then(|v| v.as_str()),
        Some("tok_12345")
    );
}

#[test]
fn test_devin_pkce_verifier_length() {
    let pkce = Pkce::generate();
    assert!((43..=128).contains(&pkce.verifier.len()));
}
