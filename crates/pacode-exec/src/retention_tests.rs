use std::sync::Arc;
use std::time::Duration;

use pacode_types::{AgentId, ExecConfig, SessionId, TaskId, TaskStatus};

use super::{MAX_STUBS, MAX_TERMINAL, Retention};
use crate::manager::{TaskManager, WaitResult};
use crate::spec::TaskSpec;

fn spec(session: &SessionId, cmd: &str) -> TaskSpec {
    TaskSpec::new(
        session.clone(),
        AgentId::generate(),
        cmd.to_string(),
        std::env::current_dir().unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_tasks_are_evicted_to_stubs_and_tail_reads_the_spool() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let session = SessionId::generate();
    let total = MAX_TERMINAL + 44;

    let mut ids: Vec<TaskId> = Vec::new();
    for batch in 0..(total / 20 + 1) {
        let mut handles = Vec::new();
        for n in 0..20 {
            let idx = batch * 20 + n;
            if idx >= total {
                break;
            }
            let mgr = Arc::clone(&mgr);
            let spec = spec(&session, &format!("echo line-{idx}"));
            handles.push(tokio::spawn(async move {
                let info = mgr.spawn(spec).await.unwrap();
                mgr.wait(&info.id, Duration::from_secs(10), false).await;
                info.id
            }));
        }
        for h in handles {
            ids.push(h.await.unwrap());
        }
    }

    // Eviction runs just after `Ended`; let the last supervisors finish.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let live = mgr.state.tasks.lock().unwrap().len();
    assert!(live <= MAX_TERMINAL, "{live} entries kept");
    assert_eq!(mgr.list(Some(&session)).len(), total);

    let evicted = ids
        .iter()
        .find(|id| !mgr.state.tasks.lock().unwrap().contains_key(*id))
        .expect("some task was evicted")
        .clone();
    let info = mgr.info(&evicted).expect("stub keeps info");
    assert_eq!(info.status, TaskStatus::Completed);
    let tail = mgr.tail(&evicted, 5).await.unwrap();
    assert_eq!(tail.len(), 1);
    assert!(tail[0].starts_with("line-"));
    assert!(matches!(
        mgr.wait(&evicted, Duration::from_secs(1), false).await,
        WaitResult::Ended(_)
    ));
    assert!(mgr.output(&evicted, 1000).await.unwrap().contains("line-"));

    mgr.ack(&evicted);
    assert!(mgr.info(&evicted).unwrap().acked);
    mgr.forget(&evicted);
    assert!(mgr.info(&evicted).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn running_tasks_are_never_evicted() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let session = SessionId::generate();
    let long = mgr.spawn(spec(&session, "sleep 30")).await.unwrap();
    for _ in 0..(MAX_TERMINAL + 5) {
        let info = mgr.spawn(spec(&session, "true")).await.unwrap();
        mgr.wait(&info.id, Duration::from_secs(10), false).await;
    }
    assert!(mgr.state.tasks.lock().unwrap().contains_key(&long.id));
    assert_eq!(mgr.info(&long.id).unwrap().status, TaskStatus::Running);
    mgr.kill(&long.id).await.unwrap();
}

#[test]
fn stub_store_is_capped_and_oldest_is_dropped() {
    let r = Retention::default();
    let session = SessionId::generate();
    let mut first = None;
    let mut infos = Vec::new();
    for i in 0..(MAX_STUBS + 3) {
        let id = TaskId::generate();
        if i == 0 {
            first = Some(id.clone());
        }
        infos.push(pacode_types::TaskInfo {
            id,
            session: session.clone(),
            owner: AgentId::generate(),
            label: String::new(),
            command: String::new(),
            cwd: std::path::PathBuf::new(),
            status: TaskStatus::Completed,
            backgrounded: false,
            exit_code: Some(0),
            started_at_ms: i as u64,
            ended_at_ms: Some(i as u64),
            progress: None,
            warnings: 0,
            errors: 0,
            output_path: std::path::PathBuf::new(),
            output_bytes: 0,
            acked: false,
        });
    }
    r.add_stubs(infos);
    assert_eq!(r.stubs(None).len(), MAX_STUBS);
    assert!(r.get(&first.unwrap()).is_none());
}
