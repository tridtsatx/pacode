use super::*;
use std::collections::HashMap;

#[test]
fn test_openai_authorize_url_query_parameters() {
    let redirect_uri = "http://localhost:1455/auth/callback";
    let challenge = "test_challenge_hash_openai";
    let state = "test_state_openai";

    let url_str = build_authorize_url(redirect_uri, challenge, state);
    let parsed = url::Url::parse(&url_str).expect("parse authorize url");

    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.host_str(), Some("auth.openai.com"));
    assert_eq!(parsed.path(), "/oauth/authorize");

    let params: HashMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
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
    assert_eq!(
        params.get("id_token_add_organizations").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        params.get("codex_cli_simplified_flow").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        params.get("originator").map(String::as_str),
        Some("codex_cli_rs")
    );

    // Verify urlencoding in raw query string
    let raw_query = parsed.query().expect("raw query");
    assert!(raw_query.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
}

#[tokio::test]
async fn test_openai_token_exchange_request_body_shape() {
    let access_payload = serde_json::json!({
        "exp": 1789000000
    });
    let mock_access_jwt = crate::flows::test_support::create_mock_jwt(&access_payload);

    let id_payload = serde_json::json!({
        "email": "dev@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "org_987654"
        }
    });
    let mock_id_jwt = crate::flows::test_support::create_mock_jwt(&id_payload);

    let success_response = serde_json::json!({
        "access_token": mock_access_jwt,
        "refresh_token": "rt_test_openai",
        "expires_in": 3600,
        "id_token": mock_id_jwt
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &success_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/oauth/token");

    let account = exchange_code_at_url(
        &mock_url,
        "code_abc",
        "verifier_xyz",
        "http://localhost:1455/auth/callback",
    )
    .await
    .expect("exchange openai code");

    assert_eq!(account.access, mock_access_jwt);
    assert_eq!(account.refresh.as_deref(), Some("rt_test_openai"));
    assert_eq!(account.email.as_deref(), Some("dev@example.com"));
    assert_eq!(account.expires_at, Some(1789000000));
    assert_eq!(
        account.extra.get("account_id").and_then(|v| v.as_str()),
        Some("org_987654")
    );

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );

    assert!(recorded.body.contains("grant_type=authorization_code"));
    assert!(recorded.body.contains(&format!("client_id={CLIENT_ID}")));
    assert!(recorded.body.contains("code=code_abc"));
    assert!(recorded.body.contains("code_verifier=verifier_xyz"));
    assert!(
        recorded
            .body
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback")
    );
}

#[tokio::test]
async fn test_openai_token_refresh_request_body_shape() {
    let access_payload = serde_json::json!({
        "exp": 1799000000
    });
    let mock_access_jwt = crate::flows::test_support::create_mock_jwt(&access_payload);

    let refresh_response = serde_json::json!({
        "access_token": mock_access_jwt,
        "refresh_token": "rt_new_token",
        "expires_in": 7200
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &refresh_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/oauth/token");

    let refreshed = refresh_tokens_at_url(&mock_url, "rt_old_token")
        .await
        .expect("refresh openai token");

    assert_eq!(refreshed.access, mock_access_jwt);
    assert_eq!(refreshed.refresh.as_deref(), Some("rt_new_token"));
    assert_eq!(refreshed.expires_at, Some(1799000000));

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );

    assert!(recorded.body.contains("grant_type=refresh_token"));
    assert!(recorded.body.contains(&format!("client_id={CLIENT_ID}")));
    assert!(recorded.body.contains("refresh_token=rt_old_token"));
}

#[test]
fn test_openai_jwt_claim_extraction() {
    let id_payload = serde_json::json!({
        "email": "agent@openai.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acc-998877"
        }
    });
    let id_jwt = crate::flows::test_support::create_mock_jwt(&id_payload);

    assert_eq!(extract_email(&id_jwt).as_deref(), Some("agent@openai.com"));
    assert_eq!(extract_account_id(&id_jwt).as_deref(), Some("acc-998877"));

    let access_payload = serde_json::json!({
        "exp": 1755555555
    });
    let access_jwt = crate::flows::test_support::create_mock_jwt(&access_payload);
    assert_eq!(expires_at_from_access_token(&access_jwt), Some(1755555555));
}

#[test]
fn test_openai_callback_input_parser() {
    let valid = "http://localhost:1455/auth/callback?code=test_code_1&state=test_state_1";
    let (code, state) = parse_callback_input_with_state(valid).expect("parse valid input");
    assert_eq!(code, "test_code_1");
    assert_eq!(state, "test_state_1");

    let query_only = "code=query_code_2&state=query_state_2";
    let (code, state) = parse_callback_input_with_state(query_only).expect("parse query input");
    assert_eq!(code, "query_code_2");
    assert_eq!(state, "query_state_2");

    let missing_code = "state=some_state";
    assert!(parse_callback_input_with_state(missing_code).is_err());

    let missing_state = "code=some_code";
    assert!(parse_callback_input_with_state(missing_state).is_err());
}
