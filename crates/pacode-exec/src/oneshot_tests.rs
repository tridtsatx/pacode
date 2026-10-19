use std::time::Duration;

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[cfg(unix)]
fn process_running(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state == "Z"),
        Err(_) => true,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_running(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
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

#[test]
fn an_unknown_status_is_never_a_success() {
    let mut out = CapturedOutput {
        status: Some(0),
        ..Default::default()
    };
    assert!(out.success());
    out.status = None;
    assert!(!out.success());
    out.status = Some(0);
    out.timed_out = true;
    assert!(!out.success());
}

#[cfg(unix)]
#[tokio::test]
async fn a_normal_exit_leaves_no_process_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("oneshot.pid");
    let script = format!("sleep 30 & echo $! > {}; printf ok", pid_file.display());

    let out = run_capture(
        "sh",
        &args(&["-c", &script]),
        Path::new("/tmp"),
        Duration::from_secs(5),
        1024,
    )
    .await
    .unwrap();
    assert!(out.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..500 {
        if !process_running(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("descendant {pid} outlived the finished capture");
}

#[cfg(unix)]
#[tokio::test]
async fn a_timeout_leaves_no_process_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("oneshot-timeout.pid");
    let script = format!(
        "sleep 30 & echo $! > {}; while :; do sleep 1; done",
        pid_file.display()
    );

    let out = run_capture(
        "sh",
        &args(&["-c", &script]),
        Path::new("/tmp"),
        Duration::from_millis(200),
        1024,
    )
    .await
    .unwrap();
    assert!(out.timed_out);
    assert!(!out.success());

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..500 {
        if !process_running(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("descendant {pid} outlived the timed out capture");
}
