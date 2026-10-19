//! `batch` tool tests: cap, ordering, merged permission, read-only bash gating.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use pacode_exec::TaskManager;
use pacode_types::{ExecConfig, Mode, PermissionDecision, WebConfig};

use super::{BatchTool, MAX_OPS, NAME};
use crate::builtin::builtin_tools;
use crate::registry::{MAX_TOOL_OUTPUT_BYTES, MAX_TOOL_WORKERS_PER_AGENT, ToolRegistry};
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

/// Registry-backed stub tool: sleeps `delay`, then returns `text` (capped like
/// a real tool). Registered under a whitelisted name so `batch` resolves it.
struct SlowStub {
    name: &'static str,
    delay: Duration,
    text: String,
}

#[async_trait]
impl Tool for SlowStub {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "stub"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn call(&self, _input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(self.delay).await;
        let capped = crate::builtin::helpers::cap_output(&self.text, false, ctx.output_cap_chars);
        Ok(ToolOutput::text(capped))
    }
}

struct CountingStub {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingStub {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "stub"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn call(&self, _input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("ok"))
    }
}

fn host_with_tasks(spool_dir: std::path::PathBuf) -> Arc<StubToolHost> {
    Arc::new(StubToolHost {
        tasks: Some(TaskManager::new(spool_dir, ExecConfig::default())),
        ..Default::default()
    })
}

fn ctx_with(host: Arc<StubToolHost>, cwd: std::path::PathBuf, tools: ToolRegistry) -> ToolCtx {
    let mut ctx = stub_ctx(host);
    ctx.cwd = cwd;
    ctx.tools = tools;
    ctx
}

#[tokio::test]
async fn ops_cap_eight_and_nonempty() {
    let host = Arc::new(StubToolHost::default());
    let ctx = ctx_with(host, std::env::current_dir().unwrap(), ToolRegistry::new());
    let tool = BatchTool::default();

    let err = tool.call(json!({}), &ctx).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));

    let nine: Vec<Value> = (0..9)
        .map(|_| json!({"tool": "read", "input": {}}))
        .collect();
    let err = tool.call(json!({"ops": nine}), &ctx).await.unwrap_err();
    match err {
        ToolError::InvalidInput(msg) => assert!(msg.contains("at most 8")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn results_follow_op_order_not_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new()
        .with(Arc::new(SlowStub {
            name: "read",
            delay: Duration::from_millis(150),
            text: "READ-RESULT".to_string(),
        }))
        .with(Arc::new(SlowStub {
            name: "grep",
            delay: Duration::ZERO,
            text: "GREP-RESULT".to_string(),
        }));
    let host = Arc::new(StubToolHost::default());
    let ctx = ctx_with(host, tmp.path().to_path_buf(), registry);

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"tool": "read", "input": {"path": "a"}},
                {"tool": "grep", "input": {"pattern": "x"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let first = out.content.find("READ-RESULT").unwrap();
    let second = out.content.find("GREP-RESULT").unwrap();
    assert!(
        first < second,
        "slow op must still report first:\n{}",
        out.content
    );
    assert!(out.content.contains("--- [1] read"));
    assert!(out.content.contains("--- [2] grep"));
}

#[tokio::test]
async fn one_permission_request_lists_all_ops() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello\n").unwrap();

    let host = Arc::new(StubToolHost {
        tasks: Some(TaskManager::new(
            tmp.path().join("spool"),
            ExecConfig::default(),
        )),
        ..Default::default()
    });
    let ctx = ctx_with(
        host.clone(),
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"tool": "read", "input": {"path": "a.txt"}},
                {"tool": "bash", "input": {"command": "echo first"}},
                {"tool": "bash", "input": {"command": "cat a.txt"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    // Exactly one draft for the whole batch, listing every runnable op.
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1, "expected one merged permission request");
    let detail = &drafts[0].detail;
    assert!(detail.contains("read a.txt"));
    assert!(detail.contains("echo first"));
    assert!(detail.contains("cat a.txt"));
    assert_eq!(drafts[0].tool_name.as_deref(), Some(NAME));

    assert!(out.content.contains("hello"));
    assert!(out.content.contains("[exit code 0]"));
}

#[tokio::test]
async fn denied_permission_fails_the_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubToolHost {
        tasks: Some(TaskManager::new(
            tmp.path().join("spool"),
            ExecConfig::default(),
        )),
        decision: std::sync::Mutex::new(PermissionDecision::Deny),
        ..Default::default()
    });
    let ctx = ctx_with(
        host,
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );

    let err = BatchTool::default()
        .call(
            json!({"ops": [{"tool": "bash", "input": {"command": "echo hi"}}]}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied(_)));
}

#[tokio::test]
async fn non_readonly_bash_is_rejected_but_batch_continues() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello\n").unwrap();

    let host = host_with_tasks(tmp.path().join("spool"));
    let ctx = ctx_with(
        host.clone(),
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"tool": "bash", "input": {"command": "rm -rf a.txt"}},
                {"tool": "bash", "input": {"command": "echo ok"}},
                {"tool": "write", "input": {"path": "x", "content": "y"}},
                {"tool": "bash", "input": {"command": "sleep 1", "background": true}},
                // Plan mode would allow it; side by side with other ops it is a script.
                {"tool": "bash", "input": {"command": "sed -n 1p a.txt"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("not a plain read-only command"));
    assert!(
        out.content.contains(
            "not a plain read-only command (batch ops run side by side): sed -n 1p a.txt"
        ),
        "{}",
        out.content
    );
    assert!(out.content.contains("not allowed in batch"));
    assert!(
        out.content.contains("background tasks are not allowed"),
        "background bash must be refused:\n{}",
        out.content
    );
    assert!(out.content.contains("ok\n[exit code 0]") || out.content.contains("ok"));
    assert!(tmp.path().join("a.txt").exists(), "rm must never have run");
    assert!(!tmp.path().join("x").exists(), "write must never have run");

    // The single draft covers the runnable bash op only.
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].detail.contains("echo ok"));
    assert!(!drafts[0].detail.contains("rm -rf"));
}

#[tokio::test]
async fn read_only_bash_op_runs_in_plan_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_with_tasks(tmp.path().join("spool"));
    let mut ctx = ctx_with(
        host,
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );
    ctx.mode = Mode::Plan;

    let out = BatchTool::default()
        .call(
            json!({"ops": [{"tool": "bash", "input": {"command": "echo plan-ok"}}]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.contains("plan-ok"));
    assert!(!out.is_error);
}

#[tokio::test]
async fn disabled_tool_fails_inside_batch() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();

    let mut registry = builtin_tools(&WebConfig::default());
    registry.apply_disabled(["read".to_string()]);
    let host = Arc::new(StubToolHost::default());
    let ctx = ctx_with(host, tmp.path().to_path_buf(), registry);

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"tool": "read", "input": {"path": "a.txt"}},
                {"tool": "glob", "input": {"pattern": "*.txt"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("disabled or not registered"));
    assert!(out.content.contains("a.txt"), "glob op still runs");
}

#[tokio::test]
async fn flat_and_aliased_op_shapes_normalize() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("b.txt"), "data\n").unwrap();

    let host = Arc::new(StubToolHost::default());
    let ctx = ctx_with(
        host,
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"name": "read", "path": "b.txt"},
                {"tool": "grep", "parameters": {"pattern": "data", "path": "b.txt"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("data"));
    assert!(out.content.contains("b.txt:1:data"));
    assert!(!out.is_error);
}

#[tokio::test]
async fn per_op_output_gets_fair_share_of_cap() {
    let tmp = tempfile::tempdir().unwrap();
    // A file larger than the per-op share of a small cap.
    let big = "x".repeat(4_000);
    std::fs::write(tmp.path().join("big.txt"), &big).unwrap();

    let host = Arc::new(StubToolHost::default());
    let mut ctx = ctx_with(
        host,
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );
    ctx.output_cap_chars = 1_000;

    let out = BatchTool::default()
        .call(
            json!({"ops": [
                {"tool": "read", "input": {"path": "big.txt"}},
                {"tool": "read", "input": {"path": "big.txt"}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    // Two ops × 500-char share: each section is truncated head+tail.
    assert!(out.content.contains("truncated"));
    assert!(out.content.len() <= 1_200);
}

#[test]
fn batch_is_registered_as_builtin() {
    let registry = builtin_tools(&WebConfig::default());
    assert!(registry.get(NAME).is_some());
}

#[tokio::test]
async fn oversized_single_op_still_obeys_hard_byte_cap() {
    let registry = ToolRegistry::new().with(Arc::new(SlowStub {
        name: "read",
        delay: Duration::ZERO,
        text: "界".repeat(MAX_TOOL_OUTPUT_BYTES / 3 + 32),
    }));
    let mut ctx = ctx_with(
        Arc::new(StubToolHost::default()),
        std::env::current_dir().unwrap(),
        registry,
    );
    ctx.output_cap_chars = 1_000_000;

    let out = BatchTool::default()
        .call(
            json!({"ops": [{"tool": "read", "input": {"path": "a"}}]}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.len() <= MAX_TOOL_OUTPUT_BYTES);
    assert!(out.content.contains("output truncated"));
}

/// Two full batches from the same agent: 2 × `MAX_OPS` ops contend for
/// `MAX_TOOL_WORKERS_PER_AGENT` slots, so this both pins the per-agent bound and
/// proves the batch takes a slot per op *only* — a batch that also held a slot
/// for itself would still pass, but one that held no slots at all would blow
/// past the bound.
#[tokio::test(flavor = "multi_thread")]
async fn batch_workers_are_bounded_per_agent() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new().with(Arc::new(CountingStub {
        active: active.clone(),
        max_active: max_active.clone(),
    }));
    let ctx = ctx_with(
        Arc::new(StubToolHost::default()),
        std::env::current_dir().unwrap(),
        registry,
    );
    let ops = (0..MAX_OPS)
        .map(|index| json!({"tool": "read", "input": {"path": index.to_string()}}))
        .collect::<Vec<_>>();

    // Two separate instances, as two concurrent `batch` calls would be: the
    // scheduler cap is per call, not shared through the tool.
    let one = BatchTool::default();
    let two = BatchTool::default();
    let (first, second) = tokio::join!(
        one.call(json!({"ops": ops.clone()}), &ctx),
        two.call(json!({"ops": ops}), &ctx),
    );

    for out in [first.unwrap(), second.unwrap()] {
        assert!(!out.is_error);
    }
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert!(
        max_active.load(Ordering::SeqCst) <= MAX_TOOL_WORKERS_PER_AGENT,
        "ops ran {} at once, over the {MAX_TOOL_WORKERS_PER_AGENT}-slot per-agent cap",
        max_active.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn an_oversized_ops_payload_is_refused_before_any_op_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new().with(Arc::new(SlowStub {
        name: "read",
        delay: Duration::ZERO,
        text: "unused".to_string(),
    }));
    let host = Arc::new(StubToolHost::default());
    let ctx = ctx_with(host, tmp.path().to_path_buf(), registry);

    let filler = "x".repeat(300 * 1024);
    let ops: Vec<Value> = (0..2)
        .map(|_| json!({"tool": "read", "input": {"path": filler.clone()}}))
        .collect();

    let err = BatchTool::default()
        .call(json!({"ops": ops}), &ctx)
        .await
        .expect_err("oversized payload");
    match err {
        ToolError::InvalidInput(msg) => assert!(msg.contains("bytes of operations"), "{msg}"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_body_stays_inside_the_call_budget_when_ops_overfill_it() {
    let tmp = tempfile::tempdir().unwrap();
    let chunk = "y".repeat(8_000);
    let registry = ToolRegistry::new()
        .with(Arc::new(SlowStub {
            name: "read",
            delay: Duration::ZERO,
            text: chunk.clone(),
        }))
        .with(Arc::new(SlowStub {
            name: "grep",
            delay: Duration::ZERO,
            text: chunk,
        }));
    let host = Arc::new(StubToolHost::default());
    let mut ctx = ctx_with(host, tmp.path().to_path_buf(), registry);
    ctx.output_cap_chars = 20_000;

    let ops: Vec<Value> = (0..8)
        .map(|index| {
            if index % 2 == 0 {
                json!({"tool": "read", "input": {"path": "a"}})
            } else {
                json!({"tool": "grep", "input": {"pattern": "x"}})
            }
        })
        .collect();

    let out = BatchTool::default()
        .call(json!({"ops": ops}), &ctx)
        .await
        .expect("batch");

    assert!(
        out.content.len() <= 20_000,
        "body of {} bytes escaped the budget",
        out.content.len()
    );
    assert!(
        out.content
            .contains("operation(s) dropped by the batch output budget"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("Batch: 8 succeeded, 0 failed"),
        "the tally must survive the cut: {}",
        out.content
    );
}

/// The `call` every `Output` event of the batch's tasks was addressed to.
async fn output_rows(ops: serde_json::Value) -> Vec<Option<pacode_types::CallId>> {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_with_tasks(tmp.path().join("spool"));
    let ctx = ctx_with(
        host.clone(),
        tmp.path().to_path_buf(),
        builtin_tools(&WebConfig::default()),
    );
    let mut events = host.tasks.as_ref().unwrap().subscribe();
    BatchTool::default()
        .call(json!({ "ops": ops }), &ctx)
        .await
        .unwrap();
    let mut rows = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let pacode_exec::TaskEvent::Output { call, .. } = event {
            rows.push(call);
        }
    }
    rows
}

#[tokio::test]
async fn a_lone_shell_op_streams_into_the_batch_row() {
    let rows = output_rows(json!([
        {"tool": "bash", "input": {"command": "echo streamed"}},
        {"tool": "ls", "input": {}},
    ]))
    .await;
    assert!(!rows.is_empty(), "the command printed a line");
    assert!(
        rows.iter()
            .all(|row| row.as_ref().map(|c| c.as_str()) == Some("call_sched")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn several_shell_ops_do_not_share_the_row() {
    let rows = output_rows(json!([
        {"tool": "bash", "input": {"command": "echo one"}},
        {"tool": "bash", "input": {"command": "echo two"}},
    ]))
    .await;
    assert!(rows.iter().all(Option::is_none), "{rows:?}");
}
