use std::time::Duration;

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn captures_stdout_and_status() {
    let out = run_capture(
        "echo",
        &args(&["hello"]),
        Path::new("/tmp"),
        Duration::from_secs(5),
        1024,
    )
    .await
    .unwrap();
    assert!(out.success());
    assert_eq!(out.status, Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    assert!(!out.stdout_truncated);
}

#[tokio::test]
async fn nonzero_exit_is_reported() {
    let out = run_capture(
        "sh",
        &args(&["-c", "echo oops >&2; exit 3"]),
        Path::new("/tmp"),
        Duration::from_secs(5),
        1024,
    )
    .await
    .unwrap();
    assert_eq!(out.status, Some(3));
    assert!(!out.success());
    assert_eq!(out.stderr_text(), "oops");
}

#[tokio::test]
async fn output_is_capped() {
    let out = run_capture(
        "sh",
        &args(&["-c", "yes | head -c 100000"]),
        Path::new("/tmp"),
        Duration::from_secs(10),
        1024,
    )
    .await
    .unwrap();
    assert!(out.stdout_truncated);
    assert_eq!(out.stdout.len(), 1024);
}

#[tokio::test]
async fn timeout_kills_the_process() {
    let out = run_capture(
        "sleep",
        &args(&["30"]),
        Path::new("/tmp"),
        Duration::from_millis(150),
        1024,
    )
    .await
    .unwrap();
    assert!(out.timed_out);
    assert_eq!(out.status, None);
}

#[tokio::test]
async fn missing_program_is_a_spawn_error() {
    let err = run_capture(
        "pacode-definitely-not-a-real-binary",
        &args(&[]),
        Path::new("/tmp"),
        Duration::from_secs(5),
        1024,
    )
    .await
    .unwrap_err();
    match err {
        ExecError::Spawn(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected Spawn, got {other:?}"),
    }
}
