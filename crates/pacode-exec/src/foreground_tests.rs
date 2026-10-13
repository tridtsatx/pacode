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

/// The next `Ended` or `Unreported` event of `id`.
async fn next_report(events: &mut broadcast::Receiver<TaskEvent>, id: &TaskId) -> TaskEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(TaskEvent::Ended(info)) if &info.id == id => return TaskEvent::Ended(info),
                Ok(TaskEvent::Unreported(info)) if &info.id == id => {
                    return TaskEvent::Unreported(info);
                }
                Ok(_) => {}
                Err(error) => panic!("event stream: {error}"),
            }
        }
    })
    .await
    .expect("task reported")
}

/// The `Ended` event of `id` — what the core's task router acts on.
async fn ended_event(events: &mut broadcast::Receiver<TaskEvent>, id: &TaskId) -> TaskInfo {
    match next_report(events, id).await {
        TaskEvent::Ended(info) => info,
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// No `Unreported` for `id` arrives within a short while.
async fn assert_no_unreported(events: &mut broadcast::Receiver<TaskEvent>, id: &TaskId) {
    let late = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            if let Ok(TaskEvent::Unreported(info)) = events.recv().await
                && &info.id == id
            {
                return info;
            }
        }
    })
    .await;
    assert!(late.is_err(), "the task must not be announced: {late:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_ending_inside_the_wait_stays_the_foregrounds_to_report() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, guard) = mgr.spawn_foreground(spec("printf done")).await.unwrap();

    let res = mgr
        .wait_foreground(&info.id, Duration::from_secs(10), false)
        .await;
    guard.delivered();

    let WaitResult::Ended(ended) = res else {
        panic!("expected Ended, got {res:?}");
    };
    assert!(!ended.backgrounded);
    assert!(!ended_event(&mut events, &info.id).await.backgrounded);
    assert_no_unreported(&mut events, &info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_that_gives_up_backgrounds_the_task_before_it_ends() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, guard) = mgr.spawn_foreground(spec("sleep 0.4")).await.unwrap();

    let res = mgr
        .wait_foreground(&info.id, Duration::from_millis(50), false)
        .await;
    guard.delivered();

    let WaitResult::Timeout(running) = res else {
        panic!("expected Timeout, got {res:?}");
    };
    assert_eq!(running.status, TaskStatus::Running);
    assert!(running.backgrounded);
    assert!(ended_event(&mut events, &info.id).await.backgrounded);
}

/// The call is dropped while its task runs — mid-wait, or before the wait
/// began (while the host wrote the task row): the task is handed over.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_dropped_while_its_task_runs_hands_it_to_the_background() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, guard) = mgr.spawn_foreground(spec("sleep 0.4")).await.unwrap();

    let id = info.id.clone();
    let call = async move {
        let _guard = guard;
        mgr.wait_foreground(&id, Duration::from_secs(10), false)
            .await
    };
    let dropped = tokio::time::timeout(Duration::from_millis(50), call).await;

    assert!(
        dropped.is_err(),
        "the call must still be pending when dropped"
    );
    assert!(ended_event(&mut events, &info.id).await.backgrounded);
    assert_no_unreported(&mut events, &info.id).await;
}

/// The window after the end: the wait saw `Ended`, but the call was dropped
/// before its result was delivered (it was reading the output back, or a
/// cancel won the same poll). Nobody told the model, so the router must.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_dropped_after_its_task_ended_leaves_it_unreported() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, guard) = mgr.spawn_foreground(spec("printf done")).await.unwrap();
    let WaitResult::Ended(_) = mgr
        .wait_foreground(&info.id, Duration::from_secs(10), false)
        .await
    else {
        panic!("task did not end");
    };
    assert!(!ended_event(&mut events, &info.id).await.backgrounded);

    drop(guard);

    let TaskEvent::Unreported(unreported) = next_report(&mut events, &info.id).await else {
        panic!("expected Unreported");
    };
    assert_eq!(unreported.status, TaskStatus::Completed);
    assert!(mgr.info(&info.id).unwrap().backgrounded);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_read_is_killed_and_never_announced() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, mut guard) = mgr.spawn_foreground(spec("sleep 30")).await.unwrap();
    guard.kill_on_drop();

    drop(guard);

    let ended = ended_event(&mut events, &info.id).await;
    assert_eq!(ended.status, TaskStatus::Killed);
    assert!(!ended.backgrounded);
    assert_no_unreported(&mut events, &info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_read_that_already_ended_is_not_announced_either() {
    let (mgr, _tmp) = manager();
    let mut events = mgr.subscribe();
    let (info, mut guard) = mgr.spawn_foreground(spec("true")).await.unwrap();
    guard.kill_on_drop();
    let _ = mgr
        .wait_foreground(&info.id, Duration::from_secs(10), false)
        .await;
    let _ = ended_event(&mut events, &info.id).await;

    drop(guard);

    assert_no_unreported(&mut events, &info.id).await;
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
