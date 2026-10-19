use std::time::Duration;

use pacode_types::{AgentId, ExecConfig, SessionId, TaskId, TaskStatus};

use super::{MAX_STUBS, MAX_TERMINAL, Retention};
use crate::manager::TaskManager;
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
