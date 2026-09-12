//! `batch` tool tests: cap, ordering, merged permission, read-only bash gating.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use pacode_exec::TaskManager;
use pacode_types::{ExecConfig, Mode, PermissionDecision, WebConfig};

use super::{BatchTool, NAME};
use crate::builtin::builtin_tools;
use crate::registry::ToolRegistry;
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
    let tool = BatchTool;

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

    let out = BatchTool
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

    let out = BatchTool
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

    let err = BatchTool
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

    let out = BatchTool
        .call(
            json!({"ops": [
                {"tool": "bash", "input": {"command": "rm -rf a.txt"}},
                {"tool": "bash", "input": {"command": "echo ok"}},
                {"tool": "write", "input": {"path": "x", "content": "y"}},
                {"tool": "bash", "input": {"command": "sleep 1", "background": true}},
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("not a read-only command"));
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

    let out = BatchTool
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

    let out = BatchTool
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

    let out = BatchTool
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

    let out = BatchTool
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
