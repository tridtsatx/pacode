use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pacode_types::{AgentId, ExecConfig, ProgressSource, SessionId, TaskProgress, TaskStatus};

use crate::manager::{ExecError, TaskEvent, TaskManager, WaitResult};
use crate::spec::TaskSpec;
use crate::state::ManagedSpool;

fn test_manager(config: ExecConfig) -> (Arc<TaskManager>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), config);
    (mgr, tmp)
}

fn test_spec(cmd: &str) -> TaskSpec {
    TaskSpec::new(
        SessionId::generate(),
        AgentId::generate(),
        cmd.to_string(),
        std::env::current_dir().unwrap(),
    )
}

#[cfg(unix)]
fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("pid file {path:?}: {error}"))
        .trim()
        .parse()
        .expect("pid")
}

#[cfg(target_os = "linux")]
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

#[cfg(unix)]
async fn wait_process_gone(pid: i32) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if !process_running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[cfg(unix)]
fn leftover_script(pid_file: &Path, tail: &str) -> String {
    format!(
        "sh -c 'sleep 30 & echo $! > {}; {tail}'",
        pid_file.display()
    )
}

/// A command that leaves behind a process which has provably escaped the direct
/// child's process group, and records that pid before the direct child exits.
///
/// The ordering is the whole point. The supervisor is entitled to signal the
/// whole process group the moment the child it owns is gone, so a grandchild
/// that has not reached `setsid` yet is still a member and is killed
/// correctly — that is not a leak, and a test must not assert otherwise. So
/// the direct child stays alive until the pid file exists: by then `setsid`
/// has already made the grandchild a session leader, and the group signal that
/// follows provably cannot reach it.
#[cfg(unix)]
fn escaped_leftover_script(pid_file: &Path) -> String {
    format!(
        "sh -c 'setsid sh -c \"echo \\$\\$ > {pid}; exec sleep 30\" & \
           while [ ! -s {pid} ]; do sleep 0.01; done; printf done'",
        pid = pid_file.display()
    )
}

#[cfg(unix)]
async fn wait_ended(mgr: &TaskManager, id: &pacode_types::TaskId) -> pacode_types::TaskInfo {
    match mgr.wait(id, Duration::from_secs(15), false).await {
        WaitResult::Ended(ended) => ended,
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_spawn_simple_output() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let spec = test_spec("printf 'a\\nb\\nc\\n'");
    let info = mgr.spawn(spec).await.unwrap();
    let res = mgr.wait(&info.id, Duration::from_secs(5), false).await;
    match res {
        WaitResult::Ended(ended) => {
            assert_eq!(ended.status, TaskStatus::Completed);
            assert_eq!(ended.exit_code, Some(0));
        }
        other => panic!("expected Ended, got {other:?}"),
    }
    let tail = mgr.tail(&info.id, 2).await.unwrap();
    assert_eq!(tail, vec!["b", "c"]);
    let spool = tokio::fs::read_to_string(&info.output_path).await.unwrap();
    assert_eq!(spool, "a\nb\nc\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_large_output_bounded_buffer() {
    let config = ExecConfig {
        tail_bytes: 4096,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let spec = test_spec("yes | head -c 300000");
    let info = mgr.spawn(spec).await.unwrap();
    let res = mgr.wait(&info.id, Duration::from_secs(5), false).await;
    match res {
        WaitResult::Ended(ended) => {
            assert_eq!(ended.status, TaskStatus::Completed);
            assert_eq!(ended.output_bytes, 300_000);
        }
        other => panic!("expected Ended, got {other:?}"),
    }
    let output = mgr.output(&info.id, 100_000).await.unwrap();
    assert!(output.contains("bytes omitted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_kill_with_zero_grace() {
    let config = ExecConfig {
        kill_grace_secs: 0,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let spec = test_spec("sleep 30");
    let info = mgr.spawn(spec).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = std::time::Instant::now();
    mgr.kill(&info.id).await.unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));

    let current = mgr.info(&info.id).unwrap();
    assert_eq!(current.status, TaskStatus::Killed);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_exit_code_failed() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let spec = test_spec("exit 3");
    let info = mgr.spawn(spec).await.unwrap();
    let res = mgr.wait(&info.id, Duration::from_secs(5), false).await;
    match res {
        WaitResult::Ended(ended) => {
            assert_eq!(ended.status, TaskStatus::Failed);
            assert_eq!(ended.exit_code, Some(3));
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_progress_from_output() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let cmd = "printf 'running 3 tests\\ntest a ... ok\\ntest b ... ok\\n'";
    let spec = test_spec(cmd);
    let info = mgr.spawn(spec).await.unwrap();
    let res = mgr.wait(&info.id, Duration::from_secs(5), false).await;
    match res {
        WaitResult::Ended(ended) => {
            let p = ended.progress.expect("expected progress");
            assert_eq!(p.current, Some(2));
            assert_eq!(p.total, Some(3));
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stall_watchdog() {
    let config = ExecConfig {
        stall_secs: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let mut rx = mgr.subscribe();
    let spec = test_spec("sleep 3");
    let info = mgr.spawn(spec).await.unwrap();

    let mut stalled_count = 0;
    let timeout = Duration::from_secs(5);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(TaskEvent::Stalled(i))) if i.id == info.id => {
                stalled_count += 1;
            }
            Ok(Ok(TaskEvent::Ended(i))) if i.id == info.id => {
                break;
            }
            _ => {}
        }
    }
    assert_eq!(stalled_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_wait_timeout() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let spec = test_spec("sleep 10");
    let info = mgr.spawn(spec).await.unwrap();
    let res = mgr.wait(&info.id, Duration::from_millis(200), false).await;
    match res {
        WaitResult::Timeout(t) => {
            assert_eq!(t.id, info.id);
            assert_eq!(t.status, TaskStatus::Running);
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
    mgr.kill(&info.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_report_progress_overrides_parsed() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let spec =
        test_spec("sh -c 'sleep 0.1; printf \"running 3 tests\\ntest a ... ok\\n\"; sleep 0.1'");
    let info = mgr.spawn(spec).await.unwrap();

    let manual_progress = TaskProgress {
        current: Some(42),
        total: Some(100),
        percent: Some(42.0),
        message: Some("manual".to_string()),
        source: ProgressSource::Reported,
        updated_at_ms: pacode_types::now_ms(),
    };
    mgr.report_progress(&info.id, manual_progress).unwrap();

    let res = mgr.wait(&info.id, Duration::from_secs(3), false).await;
    match res {
        WaitResult::Ended(ended) => {
            let p = ended.progress.expect("expected progress");
            assert_eq!(p.current, Some(42));
            assert_eq!(p.total, Some(100));
            assert_eq!(p.source, ProgressSource::Reported);
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_max_tasks_enforcement() {
    let config = ExecConfig {
        max_tasks: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let spec1 = test_spec("sleep 10");
    let info1 = mgr.spawn(spec1).await.unwrap();

    let spec2 = test_spec("sleep 10");
    let err = mgr.spawn(spec2).await.unwrap_err();
    match err {
        ExecError::TooManyTasks(max) => assert_eq!(max, 1),
        other => panic!("expected TooManyTasks, got {other:?}"),
    }

    mgr.kill(&info1.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admission_is_atomic() {
    let config = ExecConfig {
        max_tasks: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut handles = Vec::new();

    for _ in 0..2 {
        let manager = Arc::clone(&mgr);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.spawn(test_spec("sleep 10")).await
        }));
    }
    barrier.wait().await;

    let mut infos = Vec::new();
    let mut rejected = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(info) => infos.push(info),
            Err(ExecError::TooManyTasks(1)) => rejected += 1,
            Err(error) => panic!("unexpected spawn error: {error}"),
        }
    }
    assert_eq!(infos.len(), 1);
    assert_eq!(rejected, 1);
    mgr.kill(&infos[0].id).await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_kill_does_not_cancel_escalation() {
    let config = ExecConfig {
        kill_grace_secs: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let info = mgr
        .spawn(test_spec(
            "sh -c \"trap '' TERM; while :; do sleep 1; done\"",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let manager = Arc::clone(&mgr);
    let id = info.id.clone();
    let kill = tokio::spawn(async move {
        manager.kill(&id).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    kill.abort();
    let _ = kill.await;

    let ended = match tokio::time::timeout(
        Duration::from_secs(5),
        mgr.wait(&info.id, Duration::from_secs(5), false),
    )
    .await
    {
        Ok(WaitResult::Ended(info)) => info,
        other => {
            let _ = mgr.kill(&info.id).await;
            panic!("expected killed task, got {other:?}");
        }
    };
    assert_eq!(ended.status, TaskStatus::Killed);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_requests_all_kills_before_waiting() {
    let config = ExecConfig {
        max_tasks: 4,
        kill_grace_secs: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    for _ in 0..4 {
        mgr.spawn(test_spec("trap '' TERM; while :; do sleep 1; done"))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = std::time::Instant::now();
    mgr.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_retention_is_bounded_and_cleans_evicted_spools() {
    let config = ExecConfig {
        max_tasks: 1,
        ..Default::default()
    };
    let (mgr, _tmp) = test_manager(config);
    let mut infos = Vec::new();

    for _ in 0..3 {
        let info = mgr.spawn(test_spec("printf done")).await.unwrap();
        let ended = mgr.wait(&info.id, Duration::from_secs(5), false).await;
        assert!(
            matches!(ended, WaitResult::Ended(ref info) if info.status == TaskStatus::Completed)
        );
        infos.push(info);
    }

    let retained = mgr.list(None);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id, infos[2].id);
    assert!(!infos[0].output_path.exists());
    assert!(!infos[1].output_path.exists());
    assert!(infos[2].output_path.exists());

    mgr.forget(&infos[2].id);
    assert!(!infos[2].output_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn hydrated_tail_reads_only_the_bounded_tail() {
    let config = ExecConfig {
        tail_bytes: 1024,
        ..Default::default()
    };
    let (source, tmp) = test_manager(config.clone());
    let info = source.spawn(test_spec("printf done")).await.unwrap();
    source.wait(&info.id, Duration::from_secs(5), false).await;
    let ended = source.info(&info.id).unwrap();

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&ended.output_path)
        .unwrap();
    file.set_len(64 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::End(-9)).unwrap();
    file.write_all(b"old\nlast\n").unwrap();
    drop(file);

    let restored = TaskManager::new(tmp.path().to_path_buf(), config);
    restored.hydrate(vec![ended.clone()]);
    assert_eq!(
        restored.tail(&ended.id, 2).await.unwrap(),
        vec!["old", "last"]
    );
    drop(restored);
    assert!(ended.output_path.exists());
    source.shutdown().await;
}

#[test]
fn requested_spool_cleanup_waits_for_tail_leases() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("task.out");
    std::fs::write(&path, b"output").unwrap();
    let spool = Arc::new(ManagedSpool::new(path.clone(), true));
    spool.request_cleanup();
    let lease = Arc::clone(&spool);

    drop(spool);
    assert!(path.exists());
    drop(lease);
    assert!(!path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn reassign_owner_updates_live_task_identity() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let session = SessionId::generate();
    let old_owner = AgentId::main();
    let new_owner = AgentId::generate();
    let mut spec = test_spec("sleep 5");
    spec.session = session.clone();
    spec.owner = old_owner.clone();
    let info = mgr.spawn(spec).await.unwrap();

    let changed = mgr.reassign_owner(&session, &old_owner, &new_owner);
    assert_eq!(changed.len(), 1);
    assert_eq!(mgr.info(&info.id).unwrap().owner, new_owner);
    mgr.kill(&info.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn hydrate_restores_terminal_tasks_without_a_supervisor() {
    let (mgr, tmp) = test_manager(ExecConfig::default());
    let info = mgr.spawn(test_spec("printf done")).await.unwrap();
    mgr.wait(&info.id, Duration::from_secs(5), false).await;
    let ended = mgr.info(&info.id).unwrap();
    assert!(ended.status.is_terminal());

    let restored = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    restored.hydrate(vec![ended.clone()]);
    assert_eq!(restored.info(&info.id), Some(ended));
    restored.shutdown().await;
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_session_management_and_lifecycle() {
    let (mgr, _tmp) = test_manager(ExecConfig::default());
    let s1 = SessionId::generate();
    let s2 = SessionId::generate();

    let mut spec1 = test_spec("sleep 10");
    spec1.session = s1.clone();
    let info1 = mgr.spawn(spec1).await.unwrap();

    let mut spec2 = test_spec("sleep 10");
    spec2.session = s2.clone();
    let info2 = mgr.spawn(spec2).await.unwrap();

    let list_s1 = mgr.list(Some(&s1));
    assert_eq!(list_s1.len(), 1);
    assert_eq!(list_s1[0].id, info1.id);

    let list_all = mgr.list(None);
    assert_eq!(list_all.len(), 2);

    mgr.mark_backgrounded(&info1.id);
    assert!(mgr.info(&info1.id).unwrap().backgrounded);

    mgr.ack(&info1.id);
    assert!(mgr.info(&info1.id).unwrap().acked);

    mgr.kill_session(&s1).await;
    let info1_ended = mgr.info(&info1.id).unwrap();
    assert_eq!(info1_ended.status, TaskStatus::Killed);

    mgr.forget(&info1.id);
    assert!(mgr.info(&info1.id).is_none());

    mgr.shutdown().await;
    let info2_ended = mgr.info(&info2.id).unwrap();
    assert_eq!(info2_ended.status, TaskStatus::Killed);
}

#[test]
fn a_failed_wait_is_never_reported_as_success() {
    use crate::supervisor::terminal_status;

    assert_eq!(
        terminal_status(false, false, Some(0)),
        TaskStatus::Completed
    );
    assert_eq!(terminal_status(false, true, Some(0)), TaskStatus::Failed);
    assert_eq!(terminal_status(false, true, None), TaskStatus::Failed);
    assert_eq!(terminal_status(false, false, Some(1)), TaskStatus::Failed);
    assert_eq!(terminal_status(false, false, None), TaskStatus::Failed);
    assert_eq!(terminal_status(true, false, Some(0)), TaskStatus::Killed);
    assert_eq!(terminal_status(true, true, Some(0)), TaskStatus::Killed);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn natural_exit_reaps_the_process_group() {
    let (mgr, tmp) = test_manager(ExecConfig::default());
    let pid_file = tmp.path().join("natural.pid");
    let info = mgr
        .spawn(test_spec(&leftover_script(&pid_file, "printf done")))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let ended = wait_ended(&mgr, &info.id).await;
    assert_eq!(ended.status, TaskStatus::Completed);
    assert_eq!(ended.exit_code, Some(0));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(ended.exit_code.is_some());

    let descendant = read_pid(&pid_file);
    assert!(
        wait_process_gone(descendant).await,
        "descendant {descendant} outlived the finished task"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn timeout_reaps_the_process_group() {
    let (mgr, tmp) = test_manager(ExecConfig::default());
    let pid_file = tmp.path().join("timeout.pid");
    let mut spec = test_spec(&leftover_script(&pid_file, "while :; do sleep 1; done"));
    spec.timeout = Some(Duration::from_millis(200));
    let info = mgr.spawn(spec).await.unwrap();

    let ended = wait_ended(&mgr, &info.id).await;
    assert_eq!(ended.status, TaskStatus::Failed);
    assert_eq!(ended.exit_code, None);

    let descendant = read_pid(&pid_file);
    assert!(
        wait_process_gone(descendant).await,
        "descendant {descendant} outlived the timed out task"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn kill_reaps_the_process_group() {
    let (mgr, tmp) = test_manager(ExecConfig {
        kill_grace_secs: 0,
        ..Default::default()
    });
    let pid_file = tmp.path().join("killed.pid");
    let info = mgr
        .spawn(test_spec(&leftover_script(
            &pid_file,
            "while :; do sleep 1; done",
        )))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    mgr.kill(&info.id).await.unwrap();
    let current = mgr.info(&info.id).unwrap();
    assert_eq!(current.status, TaskStatus::Killed);

    let descendant = read_pid(&pid_file);
    assert!(
        wait_process_gone(descendant).await,
        "descendant {descendant} outlived the killed task"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_leaves_no_running_tasks_and_no_processes() {
    let (mgr, tmp) = test_manager(ExecConfig {
        max_tasks: 4,
        kill_grace_secs: 0,
        ..Default::default()
    });
    let mut pid_files = Vec::new();
    for index in 0..3 {
        let pid_file = tmp.path().join(format!("shutdown-{index}.pid"));
        mgr.spawn(test_spec(&leftover_script(
            &pid_file,
            "while :; do sleep 1; done",
        )))
        .await
        .unwrap();
        pid_files.push(pid_file);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    mgr.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5));

    let remaining = mgr.list(None);
    assert_eq!(remaining.len(), 3);
    assert!(remaining.iter().all(|info| info.status.is_terminal()));
    for pid_file in &pid_files {
        let descendant = read_pid(pid_file);
        assert!(
            wait_process_gone(descendant).await,
            "descendant {descendant} survived shutdown"
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_escaped_process_does_not_stall_the_task() {
    let (mgr, tmp) = test_manager(ExecConfig::default());
    let pid_file = tmp.path().join("escaped.pid");
    let info = mgr
        .spawn(test_spec(&escaped_leftover_script(&pid_file)))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let ended = wait_ended(&mgr, &info.id).await;
    assert_eq!(ended.status, TaskStatus::Completed);
    assert!(started.elapsed() < Duration::from_secs(8));

    let escaped = read_pid(&pid_file);
    assert!(process_running(escaped), "escaped process was reaped");
    assert!(unsafe { libc::kill(escaped, libc::SIGKILL) } == 0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn force_kill_falls_back_to_the_direct_child() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().expect("child pid");
    let pid = i32::try_from(pid).unwrap();
    let group = unsafe { libc::getpgid(pid) };
    assert_ne!(group, pid, "child unexpectedly leads its own process group");

    crate::supervisor::force_kill(Some(pid as u32));

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("child survived the direct kill")
        .unwrap();
    assert!(!status.success());
}
