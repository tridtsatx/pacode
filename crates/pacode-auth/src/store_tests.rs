use super::*;
use tempfile::tempdir;

#[test]
fn test_roundtrip() {
    let tmp = tempdir().expect("tempdir");
    let file_path = tmp.path().join("auth.json");

    let mut store = AuthStore::new_empty(&file_path);

    let mut extra = serde_json::Map::new();
    extra.insert(
        "api_server_url".to_string(),
        serde_json::Value::String("https://api.devin.ai".to_string()),
    );
    extra.insert(
        "org_id".to_string(),
        serde_json::Value::String("org_123".to_string()),
    );

    let devin_acc = Account {
        label: "devin-work".to_string(),
        kind: "oauth".to_string(),
        access: "access_token_xyz".to_string(),
        refresh: Some("refresh_token_abc".to_string()),
        expires_at: Some(1700000000),
        email: Some("dev@company.com".to_string()),
        extra,
    };

    store.upsert("devin", devin_acc.clone());
    store.save().expect("save store");

    let loaded = AuthStore::load_from(&file_path).expect("load store");
    let active = loaded.get("devin").expect("active devin account");
    assert_eq!(active, &devin_acc);
    assert_eq!(loaded.active_label("devin"), Some("devin-work"));
}

#[cfg(unix)]
#[test]
fn test_permissions_after_save() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let nested_dir = tmp.path().join("nested").join("sub");
    let file_path = nested_dir.join("auth.json");

    let mut store = AuthStore::new_empty(&file_path);
    store.upsert(
        "claude",
        Account {
            label: "claude-1".to_string(),
            kind: "oauth".to_string(),
            access: "tok".to_string(),
            refresh: None,
            expires_at: None,
            email: None,
            extra: serde_json::Map::new(),
        },
    );
    store.save().expect("save store");

    let file_meta = std::fs::metadata(&file_path).expect("file metadata");
    let file_mode = file_meta.permissions().mode() & 0o777;
    assert_eq!(
        file_mode, 0o600,
        "auth.json file mode should be 0600, got {:o}",
        file_mode
    );

    let dir_meta = std::fs::metadata(&nested_dir).expect("dir metadata");
    let dir_mode = dir_meta.permissions().mode() & 0o777;
    assert_eq!(
        dir_mode, 0o700,
        "directory mode should be 0700, got {:o}",
        dir_mode
    );
}

#[test]
fn test_multi_account_upsert_replace_active_semantics() {
    let tmp = tempdir().expect("tempdir");
    let file_path = tmp.path().join("auth.json");
    let mut store = AuthStore::new_empty(&file_path);

    let acc1 = Account {
        label: "acc-1".to_string(),
        kind: "oauth".to_string(),
        access: "acc1_initial".to_string(),
        refresh: None,
        expires_at: None,
        email: None,
        extra: serde_json::Map::new(),
    };
    let acc2 = Account {
        label: "acc-2".to_string(),
        kind: "oauth".to_string(),
        access: "acc2_token".to_string(),
        refresh: None,
        expires_at: None,
        email: None,
        extra: serde_json::Map::new(),
    };

    // First account becomes active automatically
    store.upsert("openai", acc1);
    assert_eq!(store.active_label("openai"), Some("acc-1"));
    assert_eq!(
        store.get("openai").map(|a| a.access.as_str()),
        Some("acc1_initial")
    );
    assert_eq!(store.accounts("openai").len(), 1);

    // Second account added does NOT take over active
    store.upsert("openai", acc2);
    assert_eq!(store.active_label("openai"), Some("acc-1"));
    assert_eq!(store.accounts("openai").len(), 2);

    // Replacing existing account by label keeps it active and updates access
    let acc1_updated = Account {
        label: "acc-1".to_string(),
        kind: "oauth".to_string(),
        access: "acc1_updated".to_string(),
        refresh: None,
        expires_at: None,
        email: None,
        extra: serde_json::Map::new(),
    };
    store.upsert("openai", acc1_updated);
    assert_eq!(store.accounts("openai").len(), 2);
    assert_eq!(store.active_label("openai"), Some("acc-1"));
    assert_eq!(
        store.get("openai").map(|a| a.access.as_str()),
        Some("acc1_updated")
    );

    // Explicitly set active account to acc-2
    store.set_active("openai", "acc-2").expect("set active");
    assert_eq!(store.active_label("openai"), Some("acc-2"));
    assert_eq!(
        store.get("openai").map(|a| a.access.as_str()),
        Some("acc2_token")
    );

    // Setting unknown account as active fails
    assert!(store.set_active("openai", "non-existent").is_err());

    // Remove active account falls back to remaining
    let removed = store.remove("openai", "acc-2");
    assert!(removed);
    assert_eq!(store.accounts("openai").len(), 1);
    assert_eq!(store.active_label("openai"), Some("acc-1"));

    // Remove last account leaves active as None
    let removed_last = store.remove("openai", "acc-1");
    assert!(removed_last);
    assert_eq!(store.accounts("openai").len(), 0);
    assert_eq!(store.active_label("openai"), None);
}

#[test]
fn test_missing_file_returns_empty_store() {
    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("does_not_exist.json");

    let store = AuthStore::load_from(&missing).expect("missing file should not fail");
    assert_eq!(store.accounts("claude").len(), 0);
    assert_eq!(store.get("claude"), None);
}

#[test]
fn test_corrupt_file_returns_error_and_preserves_content() {
    let tmp = tempdir().expect("tempdir");
    let corrupt_file = tmp.path().join("auth.json");

    let corrupt_content = "{ \"version\": 1, \"providers\": { \"broken\": [NOT VALID JSON";
    std::fs::write(&corrupt_file, corrupt_content).expect("write corrupt file");

    let result = AuthStore::load_from(&corrupt_file);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, AuthError::Store(_)));

    // File must NOT be silently deleted or altered
    assert!(corrupt_file.exists());
    let content_after = std::fs::read_to_string(&corrupt_file).expect("read corrupt file");
    assert_eq!(content_after, corrupt_content);
}
