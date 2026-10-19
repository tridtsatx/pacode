use std::time::{Duration, Instant};

use pacode_types::{AgentId, ExecConfig, SessionId};
use tokio::sync::broadcast::error::RecvError;

use super::ProgressThrottle;
use crate::manager::{TaskEvent, TaskManager};
use crate::spec::TaskSpec;

#[test]
fn throttle_emits_first_then_holds_inside_window() {
    let base = Instant::now();
    let mut t = ProgressThrottle::new(Duration::from_millis(500));
    assert!(t.on_progress(base));
    assert!(!t.on_progress(base + Duration::from_millis(10)));
    assert!(!t.on_progress(base + Duration::from_millis(499)));
    assert!(t.on_progress(base + Duration::from_millis(500)));
}

#[test]
fn throttle_owes_one_trailing_emit_then_stops() {
    let base = Instant::now();
    let mut t = ProgressThrottle::new(Duration::from_millis(500));
    assert!(t.on_progress(base));
    assert!(t.flush_deadline().is_none());
    assert!(!t.on_progress(base + Duration::from_millis(100)));
    assert!(!t.on_progress(base + Duration::from_millis(200)));
    assert_eq!(t.flush_deadline(), Some(base + Duration::from_millis(500)));
    assert!(t.on_flush(base + Duration::from_millis(500)));
    assert!(t.flush_deadline().is_none());
    assert!(!t.on_flush(base + Duration::from_millis(600)));
}

#[test]
fn throttle_caps_a_burst_to_three_per_second() {
    let base = Instant::now();
    let mut t = ProgressThrottle::new(Duration::from_millis(500));
    let mut emitted = 0;
    // 1000 progress values spread over one second (1 ms apart).
    for i in 0..1000u64 {
        if t.on_progress(base + Duration::from_millis(i)) {
            emitted += 1;
        }
    }
    assert!(emitted <= 3, "emitted {emitted}");
}

#[tokio::test(flavor = "multi_thread")]
async fn noisy_task_progress_is_coalesced() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let mut rx = mgr.subscribe();
    let spec = TaskSpec::new(
        SessionId::generate(),
        AgentId::generate(),
        "yes '1/100000' | head -n 100000",
        std::env::current_dir().unwrap(),
    );
    let info = mgr.spawn(spec).await.unwrap();
    let started = Instant::now();
    let mut progress = 0usize;
    loop {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            Ok(Ok(TaskEvent::Progress(p))) if p.id == info.id => progress += 1,
            Ok(Ok(TaskEvent::Ended(e))) if e.id == info.id => break,
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => {}
            Ok(Err(RecvError::Closed)) => panic!("event channel closed"),
            Err(_) => panic!("task did not end in time"),
        }
    }
    let secs = started.elapsed().as_secs_f64();
    assert!(
        (progress as f64) <= secs * 3.0 + 2.0,
        "{progress} Progress events in {secs:.2}s"
    );
}
