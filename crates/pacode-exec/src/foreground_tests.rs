use std::sync::Arc;
use std::time::Duration;

use pacode_types::{AgentId, ExecConfig, SessionId, TaskId, TaskInfo, TaskStatus};
use tokio::sync::broadcast;

use super::{GiveUp, background};
use crate::manager::{TaskEvent, TaskManager, WaitResult};
use crate::spec::TaskSpec;

fn manager() -> (Arc<TaskManager>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    (mgr, tmp)
}

fn spec(cmd: &str) -> TaskSpec {
    TaskSpec::new(
        SessionId::generate(),
        AgentId::generate(),
        cmd.to_string(),
        std::env::current_dir().unwrap(),
    )
}

/// The `Ended` event of `id` — what the core's task router acts on.
async fn ended_event(events: &mut broadcast::Receiver<TaskEvent>, id: &TaskId) -> TaskInfo {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(TaskEvent::Ended(info)) = events.recv().await
                && &info.id == id
            {
                return info;
            }
        }
    })
    .await
    .expect("task ended")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_ending_inside_the_wait_stays_the_foregrounds_to_report() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let info = mgr.spawn(spec("printf done")).await.unwrap();

    let res = mgr
        .wait_foreground(&info.id, Duration::from_secs(10), false)
        .await;

    let WaitResult::Ended(ended) = res else {
        panic!("expected Ended, got {res:?}");
    };
    assert!(!ended.backgrounded);
    assert!(!ended_event(&mut events, &info.id).await.backgrounded);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_that_gives_up_backgrounds_the_task_before_it_ends() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let info = mgr.spawn(spec("sleep 0.4")).await.unwrap();

    let res = mgr
        .wait_foreground(&info.id, Duration::from_millis(50), false)
        .await;

    let WaitResult::Timeout(running) = res else {
        panic!("expected Timeout, got {res:?}");
    };
    assert_eq!(running.status, TaskStatus::Running);
    assert!(running.backgrounded);
    assert!(ended_event(&mut events, &info.id).await.backgrounded);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_wait_hands_the_task_to_the_background() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let info = mgr.spawn(spec("sleep 0.4")).await.unwrap();

    let dropped = tokio::time::timeout(
        Duration::from_millis(50),
        mgr.wait_foreground(&info.id, Duration::from_secs(10), false),
    )
    .await;

    assert!(
        dropped.is_err(),
        "the wait must still be pending when dropped"
    );
    assert!(mgr.info(&info.id).unwrap().backgrounded);
    assert!(ended_event(&mut events, &info.id).await.backgrounded);
}

/// The window between the wait's deadline and the lock: a task that ended in
/// it is reported as ended, and is not flagged as backgrounded after the fact.
#[tokio::test(flavor = "multi_thread")]
async fn giving_up_on_an_ended_task_reports_it_ended() {
    let (mgr, _tmp) = manager();
    let info = mgr.spawn(spec("true")).await.unwrap();
    let WaitResult::Ended(_) = mgr.wait(&info.id, Duration::from_secs(10), false).await else {
        panic!("task did not end");
    };

    let GiveUp::Ended(ended) = background(&mgr.state, &info.id) else {
        panic!("an ended task must not be backgrounded");
    };
    assert!(!ended.backgrounded);
    assert!(!mgr.info(&info.id).unwrap().backgrounded);
}
