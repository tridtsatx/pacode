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
    assert!(
        elapsed < SLOW * 2,
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
