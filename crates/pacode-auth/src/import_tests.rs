use super::*;

fn write(dir: &std::path::Path, body: &str) -> PathBuf {
    let path = dir.join("credentials.toml");
    std::fs::write(&path, body).expect("write fixture");
    path
}

#[test]
fn missing_file_is_not_an_error() {
    let dir = std::env::temp_dir().join(format!("pacode-import-{}", std::process::id()));
    let path = dir.join("absent.toml");
    assert_eq!(
        devin_account_from(&path, "devin-1").expect("absent file is fine"),
        None
    );
}

#[test]
fn reads_key_and_urls() {
    let dir = tempdir("ok");
    let path = write(
        &dir,
        r#"
windsurf_api_key = "devin-session-token$abc.def.ghi"
api_server_url = "https://server.codeium.com"
devin_webapp_host = "app.devin.ai"
devin_api_url = "https://api.devin.ai"
"#,
    );
    let account = devin_account_from(&path, "devin-1")
        .expect("parses")
        .expect("some account");
    assert_eq!(account.label, "devin-1");
    assert_eq!(account.kind, "oauth");
    assert_eq!(account.access, "devin-session-token$abc.def.ghi");
    assert_eq!(
        account.extra.get("api_server_url").and_then(|v| v.as_str()),
        Some("https://server.codeium.com")
    );
    assert_eq!(
        account.extra.get("session_token").and_then(|v| v.as_str()),
        Some("devin-session-token$abc.def.ghi")
    );
    assert!(account.extra.contains_key("imported_from"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn empty_key_is_refused() {
    let dir = tempdir("empty");
    let path = write(&dir, "windsurf_api_key = \"\"\n");
    let err = devin_account_from(&path, "devin-1").expect_err("empty key must fail");
    assert!(matches!(err, AuthError::Denied(_)), "got {err:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unparseable_file_is_refused() {
    let dir = tempdir("bad");
    let path = write(&dir, "this is not toml = = =");
    let err = devin_account_from(&path, "devin-1").expect_err("garbage must fail");
    assert!(matches!(err, AuthError::Store(_)), "got {err:?}");
    std::fs::remove_dir_all(&dir).ok();
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pacode-import-{}-{tag}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}
