//! Ledger plumbing of the file tools: which `ToolHost` ledger calls `read`, `write`,
//! `edit` and `multi_edit` make, in which order, and that a rejection writes nothing.
//! The ledger semantics themselves are tested in `pacode-core` (`ledger_tests`).

use std::sync::Arc;

use super::*;
use crate::builtin::edit::EditTool;
use crate::builtin::multi_edit::MultiEditTool;
use crate::builtin::read::ReadTool;
use crate::hashline::line_hash;
use crate::test_support::{StubToolHost, stub_ctx};

fn setup() -> (tempfile::TempDir, Arc<StubToolHost>, ToolCtx) {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host.clone());
    ctx.cwd = tmp.path().to_path_buf();
    (tmp, host, ctx)
}

#[tokio::test]
async fn read_records_the_lines_shown() {
    let (tmp, host, ctx) = setup();
    tokio::fs::write(tmp.path().join("f.txt"), "a\nb\nc\nd\n")
        .await
        .unwrap();
    ReadTool
        .call(json!({"path": "f.txt", "offset": 2, "limit": 2}), &ctx)
        .await
        .unwrap();
    assert_eq!(host.ledger.calls(), vec!["lock 1", "read f.txt 1..3"]);
}

#[tokio::test]
async fn write_checks_before_writing_and_records_after() {
    let (tmp, host, ctx) = setup();
    WriteTool
        .call(json!({"path": "new.txt", "content": "x\n"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        host.ledger.calls(),
        vec![
            "lock 1",
            "check Write new.txt existing=false applied=true",
            "write new.txt"
        ]
    );
    assert!(tmp.path().join("new.txt").exists());
}

#[tokio::test]
async fn rejected_write_leaves_the_file_alone() {
    let (tmp, host, ctx) = setup();
    let path = tmp.path().join("f.txt");
    tokio::fs::write(&path, "old\n").await.unwrap();
    *host.ledger.reject_with.lock().unwrap() = Some("read f.txt first".to_string());

    let err = WriteTool
        .call(json!({"path": "f.txt", "content": "new\n"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(ref m) if m == "read f.txt first"),
        "{err:?}"
    );
    let err = EditTool
        .call(
            json!({"path": "f.txt", "old_string": "old", "new_string": "new"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)), "{err:?}");
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "old\n");
    assert!(!host.ledger.calls().iter().any(|c| c.starts_with("write")));
}

#[tokio::test]
async fn failed_edit_asks_the_ledger_why() {
    let (tmp, host, ctx) = setup();
    tokio::fs::write(tmp.path().join("f.txt"), "a\nb\n")
        .await
        .unwrap();
    *host.ledger.reject_with.lock().unwrap() =
        Some("f.txt was modified by agent reviewer".to_string());
    // An anchor whose hash matches no line of the file: stale.
    let bad = (0..)
        .map(|i| line_hash(&format!("x{i}")))
        .find(|h| *h != line_hash("a") && *h != line_hash("b"))
        .unwrap();
    let err = EditTool
        .call(
            json!({"path": "f.txt", "ops": [{"op": "replace", "start": format!("1#{bad}"), "lines": ["A"]}]}),
            &ctx,
        )
        .await
        .unwrap_err();
    let ToolError::InvalidInput(msg) = err else {
        panic!("expected InvalidInput");
    };
    assert!(
        msg.starts_with("f.txt was modified by agent reviewer\n\n"),
        "{msg}"
    );
    assert!(msg.contains("stale anchor"), "{msg}");
    assert_eq!(
        host.ledger.calls(),
        vec!["lock 1", "check Hashline f.txt existing=true applied=false"]
    );
}

#[tokio::test]
async fn multi_edit_locks_all_files_together() {
    let (tmp, host, ctx) = setup();
    tokio::fs::write(tmp.path().join("a.txt"), "a1\n")
        .await
        .unwrap();
    tokio::fs::write(tmp.path().join("b.txt"), "b1\n")
        .await
        .unwrap();
    MultiEditTool
        .call(
            json!({"files": [
                {"path": "a.txt", "ops": [{"op": "replace", "start": format!("1#{}", line_hash("a1")), "lines": ["A1"]}]},
                {"path": "b.txt", "ops": [{"op": "replace", "start": format!("1#{}", line_hash("b1")), "lines": ["B1"]}]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        host.ledger.calls(),
        vec![
            "lock 2",
            "check Hashline a.txt existing=true applied=true",
            "check Hashline b.txt existing=true applied=true",
            "write a.txt",
            "write b.txt"
        ]
    );
}

#[test]
fn descriptions_mention_concurrent_agents() {
    for desc in [
        WriteTool.description().to_string(),
        EditTool.description().to_string(),
        MultiEditTool.description().to_string(),
    ] {
        assert!(desc.contains("Other agents may be editing"), "{desc}");
        assert!(
            desc.contains("re-read only the indicated line ranges"),
            "{desc}"
        );
    }
}
