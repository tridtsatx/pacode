use std::sync::Arc;

use super::*;
use crate::hashline::line_hash;
use crate::test_support::{StubToolHost, stub_ctx};

fn ctx_in(dir: &Path) -> ToolCtx {
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = dir.to_path_buf();
    ctx
}

fn anchor(content: &str, line: usize) -> String {
    let t = content.lines().nth(line - 1).unwrap_or("");
    format!("{line}#{}", line_hash(t))
}

#[tokio::test]
async fn edits_several_files_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let a = "a1\na2\n";
    let b = "b1\nb2\nb3\n";
    tokio::fs::write(tmp.path().join("a.txt"), a).await.unwrap();
    tokio::fs::write(tmp.path().join("b.txt"), b).await.unwrap();

    let out = MultiEditTool
        .call(
            json!({"files": [
                {"path": "a.txt", "ops": [{"op": "replace", "start": anchor(a, 1), "lines": ["A1"]}]},
                {"path": "b.txt", "ops": [{"op": "delete", "start": anchor(b, 2), "end": anchor(b, 3)}]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.starts_with("Multi-edited 2 file(s):"));
    assert_eq!(
        tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap(),
        "A1\na2\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(tmp.path().join("b.txt"))
            .await
            .unwrap(),
        "b1\n"
    );
    assert_eq!(out.diff.map(|d| (d.added, d.removed)), Some((1, 3)));
}

#[tokio::test]
async fn one_stale_anchor_writes_nothing_and_reports_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let a = "a1\na2\n";
    let b = "b1\nb2\n";
    tokio::fs::write(tmp.path().join("a.txt"), a).await.unwrap();
    tokio::fs::write(tmp.path().join("b.txt"), "b1\nCHANGED\n")
        .await
        .unwrap();
    tokio::fs::write(tmp.path().join("c.txt"), "c1\nc2\n")
        .await
        .unwrap();

    let err = MultiEditTool
        .call(
            json!({"files": [
                {"path": "a.txt", "ops": [{"op": "replace", "start": anchor(a, 1), "lines": ["A1"]}]},
                {"path": "b.txt", "ops": [{"op": "delete", "start": anchor(b, 2)}]},
                {"path": "c.txt", "ops": [{"op": "delete", "start": "1#zz"}]}
            ]}),
            &ctx,
        )
        .await
        .unwrap_err();
    let ToolError::InvalidInput(msg) = err else {
        panic!("expected InvalidInput");
    };
    assert!(msg.contains("b.txt:") && msg.contains("c.txt:"), "{msg}");
    assert!(msg.contains("No file was written"));
    assert!(msg.contains(&format!("2#{}| CHANGED", line_hash("CHANGED"))));
    assert_eq!(
        tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap(),
        a
    );
}

#[tokio::test]
async fn duplicate_paths_and_mixed_forms_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    tokio::fs::write(tmp.path().join("a.txt"), "x\n")
        .await
        .unwrap();
    let op = json!({"op": "insert_after", "at": "EOF", "lines": ["y"]});
    let dup = MultiEditTool
        .call(
            json!({"files": [
                {"path": "a.txt", "ops": [op.clone()]},
                {"path": "a.txt", "ops": [op.clone()]}
            ]}),
            &ctx,
        )
        .await;
    assert!(matches!(dup, Err(ToolError::InvalidInput(_))));
    let mixed = MultiEditTool
        .call(
            json!({"files": [{"path": "a.txt", "ops": [op]}], "path": "a.txt",
                   "edits": [{"old_string": "x", "new_string": "z"}]}),
            &ctx,
        )
        .await;
    assert!(matches!(mixed, Err(ToolError::InvalidInput(_))));
    assert_eq!(
        tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap(),
        "x\n"
    );
}

#[tokio::test]
async fn legacy_path_edits_form_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    tokio::fs::write(tmp.path().join("m.txt"), "aaa\nbbb\n")
        .await
        .unwrap();
    let out = MultiEditTool
        .call(
            json!({"path": "m.txt", "edits": [
                {"old_string": "aaa", "new_string": "AAA"},
                {"old_string": "bbb", "new_string": "BBB"}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.contains("Edit #2: 1 replacement(s)"));
    assert_eq!(
        tokio::fs::read_to_string(tmp.path().join("m.txt"))
            .await
            .unwrap(),
        "AAA\nBBB\n"
    );
}

#[test]
fn schema_has_files_and_legacy_fields() {
    let s = MultiEditTool.schema();
    assert!(s["properties"]["files"]["items"]["properties"]["ops"].is_object());
    assert!(s["properties"]["edits"].is_object());
    assert!(s.get("required").is_none());
}
