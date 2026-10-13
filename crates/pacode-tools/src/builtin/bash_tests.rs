use std::path::Path;
use std::sync::Arc;

use pacode_types::state::PermissionDecision;
use pacode_types::{Mode, RiskLevel};
use serde_json::json;

use super::BashTool;
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolError};

fn concurrent(command: &str) -> bool {
    BashTool::default().runs_concurrently(&json!({ "command": command }), Path::new("/work/repo"))
}

#[test]
fn reads_that_need_no_prompt_run_alongside_each_other() {
    for command in [
        "git status --short",
        "git log --oneline -5",
        "ls -la crates",
        "rg -n foo src",
        "wc -l src/main.rs",
        "cat Cargo.toml | head -20",
        "timeout 0.3 tail -f /dev/null; echo done",
    ] {
        assert!(concurrent(command), "{command} only reads");
    }
}

#[test]
fn anything_that_may_write_or_ask_keeps_its_place() {
    for command in [
        "rm -rf build",
        "cargo build",
        "echo hi > out.txt",
        "sed -i s/a/b/ file",
        "git commit -m x",
        "cat .env",
        "cat ../../secret",
        "find . -delete",
        "ls $(whoami)",
        "ls & touch x",
        "ls &>/dev/null touch x",
        "{ ls; touch x; }",
        "",
    ] {
        assert!(!concurrent(command), "{command:?} must stay sequential");
    }
    assert!(
        !BashTool::default().runs_concurrently(&json!({}), Path::new("/work")),
        "no command, nothing to prove"
    );
}

/// The call never reaches a task: the stub host has none, and the decision it
/// returns is a denial, so even a misclassified command could not run.
fn denying_host() -> Arc<StubToolHost> {
    let host = Arc::new(StubToolHost::default());
    *host.decision.lock().unwrap() = PermissionDecision::Deny;
    host
}

#[tokio::test]
async fn a_backgrounded_write_is_not_a_plan_mode_read() {
    let host = denying_host();
    let mut ctx = stub_ctx(host.clone());
    ctx.mode = Mode::Plan;

    let err = BashTool::default()
        .call(json!({ "command": "ls & touch x" }), &ctx)
        .await
        .unwrap_err();

    assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
    assert!(
        host.drafts.lock().unwrap().is_empty(),
        "denied before any prompt"
    );
}

#[tokio::test]
async fn a_command_after_an_ampersand_reaches_the_permission_check_with_its_risk() {
    for (command, level) in [
        ("ls & rm -rf ~", RiskLevel::Catastrophic),
        ("ls |& rm -rf ~", RiskLevel::Catastrophic),
        ("{ rm -rf ~; }", RiskLevel::Catastrophic),
        ("X=1 rm -rf ~", RiskLevel::Catastrophic),
        ("echo \"$(rm -rf ~)\"", RiskLevel::Catastrophic),
    ] {
        let host = denying_host();
        let ctx = stub_ctx(host.clone());

        let result = BashTool::default()
            .call(json!({ "command": command }), &ctx)
            .await;

        assert!(result.is_err(), "{command:?} must not run");
        let drafts = host.drafts.lock().unwrap();
        assert_eq!(drafts.len(), 1, "{command:?}");
        assert_eq!(drafts[0].risk, Some(level), "{command:?}");
    }
}
