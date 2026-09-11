use super::*;

#[tokio::test]
async fn test_access_token_fresh_token_untouched() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs() as i64;

    let account = Account {
        label: "claude-1".to_string(),
        kind: "oauth".to_string(),
        access: "fresh_access_token".to_string(),
        refresh: Some("my_refresh_token".to_string()),
        expires_at: Some(now_secs + 3600), // 1 hour in future
        email: None,
        extra: serde_json::Map::new(),
    };

    store.upsert("claude", account);

    let token = access_token_in(&mut store, "claude")
        .await
        .expect("get fresh token");

    assert_eq!(token, "fresh_access_token");
}

#[tokio::test]
async fn test_access_token_expired_token_refreshed_once() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs() as i64;

    let account = Account {
        label: "claude-1".to_string(),
        kind: "oauth".to_string(),
        access: "stale_access_token".to_string(),
        refresh: Some("active_refresh_token".to_string()),
        expires_at: Some(now_secs - 100), // expired 100s ago
        email: Some("user@test.com".to_string()),
        extra: serde_json::Map::new(),
    };
    store.upsert("claude", account);

    let refresh_response = serde_json::json!({
        "access_token": "brand_new_access_token",
        "refresh_token": "next_refresh_token",
        "expires_in": 3600
    })
    .to_string();

    let (port, handle) = crate::flows::test_support::mock_server(200, &refresh_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/oauth/token");

    let token = access_token_in_with_endpoint(&mut store, "claude", Some(&mock_url))
        .await
        .expect("refresh expired token");

    assert_eq!(token, "brand_new_access_token");

    let recorded = handle.await.expect("join handle");
    assert_eq!(recorded.method, "POST");

    // Verify store was mutated with new token and new expiry
    let updated = store.get("claude").expect("updated account");
    assert_eq!(updated.access, "brand_new_access_token");
    assert_eq!(updated.refresh.as_deref(), Some("next_refresh_token"));
    assert!(updated.expires_at.unwrap() >= now_secs + 3500);
}

#[tokio::test]
async fn test_access_token_expiring_within_60s_refreshes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs() as i64;

    let account = Account {
        label: "claude-soon".to_string(),
        kind: "oauth".to_string(),
        access: "about_to_expire".to_string(),
        refresh: Some("active_refresh_token".to_string()),
        expires_at: Some(now_secs + 30), // expires in 30s (within 60s window)
        email: None,
        extra: serde_json::Map::new(),
    };
    store.upsert("claude", account);

    let refresh_response = serde_json::json!({
        "access_token": "renewed_access_token",
        "expires_in": 3600
    })
    .to_string();

    let (port, _handle) = crate::flows::test_support::mock_server(200, &refresh_response).await;
    let mock_url = format!("http://127.0.0.1:{port}/oauth/token");

    let token = access_token_in_with_endpoint(&mut store, "claude", Some(&mock_url))
        .await
        .expect("refresh token expiring within 60s");

    assert_eq!(token, "renewed_access_token");
}

#[tokio::test]
async fn test_access_token_refresh_failure_surfaced_and_terminal() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_secs() as i64;

    let account = Account {
        label: "claude-term".to_string(),
        kind: "oauth".to_string(),
        access: "old_token".to_string(),
        refresh: Some("revoked_refresh_token".to_string()),
        expires_at: Some(now_secs - 10),
        email: None,
        extra: serde_json::Map::new(),
    };
    store.upsert("claude", account);

    clear_refresh_state_for_test("claude:claude-term");

    let error_body = serde_json::json!({
        "error": "invalid_grant",
        "error_description": "Refresh token is invalid or has expired."
    })
    .to_string();

    let (port, _handle) = crate::flows::test_support::mock_server(400, &error_body).await;
    let mock_url = format!("http://127.0.0.1:{port}/oauth/token");

    // First call: hits mock server, fails, records terminal error
    let err1 = access_token_in_with_endpoint(&mut store, "claude", Some(&mock_url))
        .await
        .expect_err("should fail with invalid_grant");
    assert!(format!("{err1}").contains("invalid_grant"));

    // Second call: does NOT hit network, immediately blocked by terminal failure in RefreshState
    let err2 = access_token_in_with_endpoint(&mut store, "claude", Some(&mock_url))
        .await
        .expect_err("should immediately fail due to terminal state");
    assert!(format!("{err2}").contains("terminal failure"));
}

#[tokio::test]
async fn test_access_token_devin_returns_key_unchanged_without_network() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let account = Account {
        label: "devin-1".to_string(),
        kind: "oauth".to_string(),
        access: "devin_api_key_secret_val".to_string(),
        refresh: None,
        expires_at: None,
        email: None,
        extra: serde_json::Map::new(),
    };
    store.upsert("devin", account);

    let token = access_token_in(&mut store, "devin")
        .await
        .expect("get devin token");

    assert_eq!(token, "devin_api_key_secret_val");
}

#[tokio::test]
async fn test_access_token_unknown_provider_error() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut store = AuthStore::new_empty(temp.path().join("auth.json"));

    let res = access_token_in(&mut store, "nonexistent").await;
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), AuthError::UnknownProvider(_)));
}

#[tokio::test]
async fn test_login_unknown_provider_error() {
    let cb = Arc::new(|_| {});
    let res = login("unsupported_xyz", cb).await;
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), AuthError::UnknownProvider(_)));
}

#[test]
fn test_auth_client_proxy_resolution_and_validation() {
    // 1. Provider proxy wins over global
    let cfg: pacode_types::Config = serde_json::from_value(serde_json::json!({
        "provider": { "proxy": "http://global:8080" },
        "providers": {
            "claude": { "proxy": "http://prov:8080", "base_url": "https://api.anthropic.com" }
        }
    }))
    .unwrap();
    let client = auth_client_from_config(&cfg, "claude");
    assert!(client.is_ok());

    // 2. Global applies when provider has none
    let cfg_global: pacode_types::Config = serde_json::from_value(serde_json::json!({
        "provider": { "proxy": "http://global:8080" },
        "providers": {
            "openai": { "base_url": "https://api.openai.com" }
        }
    }))
    .unwrap();
    let client = auth_client_from_config(&cfg_global, "openai");
    assert!(client.is_ok());

    // 3. Provider "none" overrides global
    let cfg_disabled: pacode_types::Config = serde_json::from_value(serde_json::json!({
        "provider": { "proxy": "http://global:8080" },
        "providers": {
            "devin": { "proxy": "none", "base_url": "https://api.devin.ai" }
        }
    }))
    .unwrap();
    let client = auth_client_from_config(&cfg_disabled, "devin");
    assert!(client.is_ok());

    // 4. Malformed proxy produces AuthError::Config naming provider and offending value
    let cfg_bad: pacode_types::Config = serde_json::from_value(serde_json::json!({
        "providers": {
            "claude": { "proxy": "unsupported-scheme://host:9090", "base_url": "https://api.anthropic.com" }
        }
    }))
    .unwrap();
    let res = auth_client_from_config(&cfg_bad, "claude");
    assert!(res.is_err());
    match res {
        Err(AuthError::Config(msg)) => {
            assert!(msg.contains("claude"), "error must name provider: {msg}");
            assert!(
                msg.contains("unsupported-scheme://host:9090"),
                "error must name offending value: {msg}"
            );
        }
        other => panic!("expected AuthError::Config, got {other:?}"),
    }
}

#[tokio::test]
async fn test_auth_client_routes_through_proxy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    let proxy_handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = "{\"access_token\":\"mock_token\",\"expires_in\":3600}";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
        req_str
    });

    let cfg: pacode_types::Config = serde_json::from_value(serde_json::json!({
        "providers": {
            "claude": { "proxy": format!("http://{proxy_addr}"), "base_url": "https://api.anthropic.com" }
        }
    }))
    .unwrap();

    let client = auth_client_from_config(&cfg, "claude").expect("client builds");
    let resp = client
        .post("http://anthropic.auth.test:80/token")
        .send()
        .await;
    assert!(resp.is_ok());

    let received = proxy_handle.await.unwrap();
    assert!(
        received.starts_with("POST http://anthropic.auth.test/token HTTP/1.1"),
        "expected auth request to reach proxy with absolute URI, got: {received}"
    );
}
