//! Read-only `bash` calls of one step run together; the rest keep their place.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pacode_tools::builtin::bash::BashTool;
use pacode_types::{CallId, Config, Mode};
use tokio_util::sync::CancellationToken;

use super::execute_tool_calls;
use super::tools_tests::{test_session, tool_results};
use crate::turn::stream::PendingToolCall;

/// A read-only command the classifier calls `Safe` that takes `SLOW`.
const SLOW_READ: &str = "timeout 0.3 tail -f /dev/null";
const SLOW: Duration = Duration::from_millis(300);

fn bash(id: &str, command: &str) -> PendingToolCall {
    PendingToolCall {
        call_id: CallId::new(format!("call-{id}")),
        name: "bash".to_string(),
        input: serde_json::json!({"command": command, "intent": id}),
        parse_err: None,
        item_seq: None,
    }
}

/// A session whose row exists in the store: a task row references it.
async fn bash_session() -> (
    tempfile::TempDir,
    Arc<crate::session::Session>,
    Arc<crate::agent::Agent>,
) {
    let (temp, session, agent) = test_session(Config::default(), Arc::new(BashTool::default()));
    session.store.upsert_session(&session.meta()).await.unwrap();
    (temp, session, agent)
}

/// The text of each result, without the `[exit code …]` line.
fn outputs(agent: &crate::agent::Agent) -> Vec<String> {
    tool_results(agent)
        .iter()
        .map(|result| result.lines().next().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_commands_of_one_step_run_together_in_call_order() {
    let (_temp, session, agent) = bash_session().await;

    let started = Instant::now();
    let interrupted = execute_tool_calls(
        &session,
        &agent,
        &CancellationToken::new(),
        ["one", "two", "three"]
            .iter()
            .map(|id| bash(id, &format!("{SLOW_READ}; echo {id}")))
            .collect(),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(!interrupted);
    assert_eq!(outputs(&agent), ["one", "two", "three"]);
    // One after another they cannot take less than 3 × SLOW; side by side
    // they take one SLOW plus overhead, which a loaded machine stretches.
    assert!(
        elapsed < SLOW * 3,
        "three {SLOW:?} reads took {elapsed:?}: they ran one after another"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_writes_splits_the_reads_around_it() {
    let (temp, session, agent) = bash_session().await;
    // Bypass, so the write runs without a prompt: what is under test is its
    // place in the sequence, not the permission.
    session.meta.write().unwrap().mode = Mode::Bypass;
    let marker = temp.path().join("marker.txt");

    let started = Instant::now();
    execute_tool_calls(
        &session,
        &agent,
        &CancellationToken::new(),
        vec![
            bash("before", &format!("{SLOW_READ}; echo before")),
            bash("write", "echo written > marker.txt"),
            bash("after", &format!("{SLOW_READ}; cat marker.txt")),
        ],
    )
    .await;
    let elapsed = started.elapsed();

    assert!(marker.exists());
    assert_eq!(outputs(&agent), ["before", "", "written"]);
    assert!(
        elapsed >= SLOW * 2,
        "the reads on either side of a write cannot overlap it ({elapsed:?})"
    );
}

/// Every task of the session, once each has ended.
async fn ended_tasks(
    session: &crate::session::Session,
    count: usize,
) -> Vec<pacode_types::TaskInfo> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let tasks = session.tasks.list(Some(&session.id));
            if tasks.len() == count && tasks.iter().all(|t| t.status.is_terminal()) {
                return tasks;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("every task ends")
}

/// Cancel the step once `count` of its tasks are running: the interrupt
/// lands mid-command however long the machine takes to start them.
async fn interrupt_when_running(
    session: &crate::session::Session,
    cancel: &CancellationToken,
    count: usize,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while session
            .tasks
            .list(Some(&session.id))
            .iter()
            .filter(|t| t.status == pacode_types::TaskStatus::Running)
            .count()
            < count
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the step's tasks start");
    cancel.cancel();
}

/// Esc during a step of reads: nobody needs their output any more, so they
/// are killed — not left to finish and announce themselves one by one.
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_reads_are_killed_not_announced() {
    let (_temp, session, agent) = bash_session().await;
    let cancel = CancellationToken::new();
    let interrupt = interrupt_when_running(&session, &cancel, 3);

    let (interrupted, ()) = tokio::join!(
        execute_tool_calls(
            &session,
            &agent,
            &cancel,
            ["one", "two", "three"]
                .iter()
                .map(|id| bash(id, "timeout 30 tail -f /dev/null"))
                .collect(),
        ),
        interrupt
    );

    assert!(interrupted);
    for task in ended_tasks(&session, 3).await {
        assert_eq!(task.status, pacode_types::TaskStatus::Killed, "{task:?}");
        assert!(!task.backgrounded, "a killed read is never announced");
    }
}

/// An interrupted command that may write is not killed half way: it is
/// handed to the background and finishes (its end is announced later).
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_write_finishes_in_the_background() {
    let (temp, session, agent) = bash_session().await;
    session.meta.write().unwrap().mode = Mode::Bypass;
    let cancel = CancellationToken::new();
    let interrupt = interrupt_when_running(&session, &cancel, 1);

    let (interrupted, ()) = tokio::join!(
        execute_tool_calls(
            &session,
            &agent,
            &cancel,
            vec![bash("write", "sleep 0.4; echo done > marker.txt")],
        ),
        interrupt
    );

    assert!(interrupted);
    let tasks = ended_tasks(&session, 1).await;
    assert_eq!(tasks[0].status, pacode_types::TaskStatus::Completed);
    assert!(tasks[0].backgrounded, "its end is the router's to announce");
    assert!(temp.path().join("marker.txt").exists());
}
