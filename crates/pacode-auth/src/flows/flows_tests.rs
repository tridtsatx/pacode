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
