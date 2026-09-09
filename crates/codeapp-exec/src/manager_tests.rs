use std::sync::Arc;
use std::time::Duration;

use codeapp_types::{AgentId, ExecConfig, ProgressSource, SessionId, TaskProgress, TaskStatus};

use crate::manager::{ExecError, TaskEvent, TaskManager, WaitResult};
use crate::spec::TaskSpec;

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
        updated_at_ms: codeapp_types::now_ms(),
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
