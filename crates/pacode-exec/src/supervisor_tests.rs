use std::time::{Duration, Instant};

use pacode_types::{AgentId, ExecConfig, SessionId};
use tokio::sync::broadcast::error::RecvError;

use super::{OUTPUT_EMIT_INTERVAL, OutputCoalescer, ProgressThrottle};
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

#[test]
fn output_first_batch_goes_out_then_text_is_held_and_flushed() {
    let base = Instant::now();
    let mut c = OutputCoalescer::new(Duration::from_millis(200), 1024);
    assert_eq!(c.push("a\n", base).as_deref(), Some("a\n"));
    assert!(c.flush_deadline().is_none());
    assert_eq!(c.push("b\n", base + Duration::from_millis(10)), None);
    assert_eq!(c.push("c\n", base + Duration::from_millis(20)), None);
    assert_eq!(c.flush_deadline(), Some(base + Duration::from_millis(200)));
    assert_eq!(
        c.on_flush(base + Duration::from_millis(200)).as_deref(),
        Some("b\nc\n")
    );
    assert!(c.flush_deadline().is_none());
    assert_eq!(c.on_flush(base + Duration::from_millis(400)), None);
    assert_eq!(c.take_rest(), None);
}

#[test]
fn output_over_the_size_cap_goes_out_at_once() {
    let base = Instant::now();
    let mut c = OutputCoalescer::new(Duration::from_millis(200), 8);
    assert!(c.push("1\n", base).is_some());
    assert_eq!(c.push("2345\n", base + Duration::from_millis(1)), None);
    assert_eq!(
        c.push("6789\n", base + Duration::from_millis(2)).as_deref(),
        Some("2345\n6789\n")
    );
    assert!(c.flush_deadline().is_none());
}

#[test]
fn held_output_is_handed_over_at_the_end() {
    let base = Instant::now();
    let mut c = OutputCoalescer::new(Duration::from_millis(200), 1024);
    assert!(c.push("x\n", base).is_some());
    assert_eq!(c.push("y\n", base + Duration::from_millis(5)), None);
    assert_eq!(c.take_rest().as_deref(), Some("y\n"));
    assert_eq!(c.take_rest(), None);
    assert!(c.flush_deadline().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn noisy_task_output_is_coalesced_and_complete_before_ended() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let mut rx = mgr.subscribe();
    // Many small writes, spread over about a second.
    let spec = TaskSpec::new(
        SessionId::generate(),
        AgentId::generate(),
        "i=0; while [ $i -lt 200 ]; do echo line$i; i=$((i+1)); sleep 0.005; done",
        std::env::current_dir().unwrap(),
    );
    let info = mgr.spawn(spec).await.unwrap();
    let started = Instant::now();
    let mut events = 0usize;
    let mut text = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            Ok(Ok(TaskEvent::Output {
                info: i, text: t, ..
            })) if i.id == info.id => {
                events += 1;
                text.push_str(&t);
            }
            Ok(Ok(TaskEvent::Ended(e))) if e.id == info.id => break,
            Ok(Ok(_)) => {}
            Ok(Err(RecvError::Lagged(n))) => panic!("lagged by {n}"),
            Ok(Err(RecvError::Closed)) => panic!("event channel closed"),
            Err(_) => panic!("task did not end in time"),
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let expected: String = (0..200).map(|i| format!("line{i}\n")).collect();
    assert_eq!(text, expected, "every line arrives, in order, before Ended");
    assert!(
        (events as f64) <= secs / OUTPUT_EMIT_INTERVAL.as_secs_f64() + 3.0,
        "{events} Output events in {secs:.2}s"
    );
}
