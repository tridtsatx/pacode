use std::sync::Arc;

use super::*;
use crate::builtin::read::ReadTool;
use crate::hashline::line_hash;
use crate::test_support::{StubToolHost, stub_ctx};

fn ctx_in(dir: &Path) -> ToolCtx {
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = dir.to_path_buf();
    ctx
}

fn anchor(text: &str, line: usize, content: &str) -> String {
    let t = content.lines().nth(line - 1).unwrap_or(text);
    format!("{line}#{}", line_hash(t))
}

#[tokio::test]
async fn hashline_replace_insert_delete_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let src = "alpha\nbeta\ngamma\ndelta\n";
    tokio::fs::write(tmp.path().join("f.txt"), src)
        .await
        .unwrap();

    let out = EditTool
        .call(
            json!({"path": "f.txt", "ops": [
                {"op": "replace", "start": anchor("", 2, src), "lines": ["BETA", "BETA2"]},
                {"op": "delete", "start": anchor("", 3, src)},
                {"op": "insert_after", "at": "EOF", "lines": ["omega"]},
                {"op": "insert_before", "at": "bof", "lines": ["first"]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    let now = tokio::fs::read_to_string(tmp.path().join("f.txt"))
        .await
        .unwrap();
    assert_eq!(now, "first\nalpha\nBETA\nBETA2\ndelta\nomega\n");
    assert!(out.content.starts_with("Edited f.txt: 4 op(s) applied"));
    // fresh anchors for the changed lines are included
    assert!(
        out.content
            .contains(&format!("3#{}| BETA", line_hash("BETA")))
    );
    assert!(!tmp.path().join("f.txt.pacode-tmp").exists());
    assert_eq!(out.diff.map(|d| (d.added, d.removed)), Some((4, 2)));
}

#[tokio::test]
async fn read_output_anchors_feed_edit_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    tokio::fs::write(tmp.path().join("g.rs"), "fn a() {}\nfn b() {}\n")
        .await
        .unwrap();
    let read = ReadTool.call(json!({"path": "g.rs"}), &ctx).await.unwrap();
    let second = read.content.lines().nth(1).unwrap();
    assert_eq!(second, format!("2#{}| fn b() {{}}", line_hash("fn b() {}")));
    // the whole display row is accepted as an anchor
    EditTool
        .call(
            json!({"path": "g.rs", "ops": [
                {"op": "replace", "start": second, "lines": ["fn c() {}"]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    let now = tokio::fs::read_to_string(tmp.path().join("g.rs"))
        .await
        .unwrap();
    assert_eq!(now, "fn a() {}\nfn c() {}\n");
}

#[tokio::test]
async fn stale_anchor_rejects_and_returns_fresh_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let before = "one\ntwo\nthree\n";
    let path = tmp.path().join("s.txt");
    tokio::fs::write(&path, "one\nTWO!\nthree\n").await.unwrap();

    let err = EditTool
        .call(
            json!({"path": "s.txt", "ops": [
                {"op": "replace", "start": anchor("", 1, before), "lines": ["ONE"]},
                {"op": "delete", "start": anchor("", 2, before)}
            ]}),
            &ctx,
        )
        .await
        .unwrap_err();
    let ToolError::InvalidInput(msg) = err else {
        panic!("expected InvalidInput");
    };
    assert!(msg.contains("nothing written"), "{msg}");
    assert!(
        msg.contains(&format!("2#{}| TWO!", line_hash("TWO!"))),
        "{msg}"
    );
    // nothing was applied, not even the valid first op
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        "one\nTWO!\nthree\n"
    );
}

#[tokio::test]
async fn drift_is_relocated_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let before = "a\nb\nc\n";
    tokio::fs::write(tmp.path().join("d.txt"), "x\ny\na\nb\nc\n")
        .await
        .unwrap();
    let out = EditTool
        .call(
            json!({"path": "d.txt", "ops": [
                {"op": "replace", "start": anchor("", 2, before), "lines": ["B"]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("had moved; matched by hash at line 4"),
        "{}",
        out.content
    );
    let now = tokio::fs::read_to_string(tmp.path().join("d.txt"))
        .await
        .unwrap();
    assert_eq!(now, "x\ny\na\nB\nc\n");
}

#[tokio::test]
async fn crlf_and_missing_final_newline_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let src = "one\r\ntwo\r\nthree";
    tokio::fs::write(tmp.path().join("w.txt"), src)
        .await
        .unwrap();
    EditTool
        .call(
            json!({"path": "w.txt", "ops": [
                {"op": "replace", "start": anchor("", 3, src), "lines": ["THREE"]},
                {"op": "insert_after", "at": anchor("", 1, src), "lines": ["one-b"]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    let now = tokio::fs::read(tmp.path().join("w.txt")).await.unwrap();
    assert_eq!(now, b"one\r\none-b\r\ntwo\r\nTHREE");
}

#[cfg(unix)]
#[tokio::test]
async fn file_mode_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    let path = tmp.path().join("run.sh");
    tokio::fs::write(&path, "echo hi\n").await.unwrap();
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    EditTool
        .call(
            json!({"path": "run.sh", "ops": [
                {"op": "replace", "start": anchor("", 1, "echo hi\n"), "lines": ["echo bye"]}
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    let mode = tokio::fs::metadata(&path)
        .await
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[tokio::test]
async fn legacy_form_still_works_and_forms_cannot_mix() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    tokio::fs::write(tmp.path().join("l.txt"), "hello\n")
        .await
        .unwrap();
    let out = EditTool
        .call(
            json!({"path": "l.txt", "old_string": "hello", "new_string": "bye"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(out.content, "Edited l.txt: 1 replacement(s)");

    let both = EditTool
        .call(
            json!({"path": "l.txt", "old_string": "bye", "new_string": "x",
                   "ops": [{"op": "delete", "start": "1#00"}]}),
            &ctx,
        )
        .await;
    assert!(matches!(both, Err(ToolError::InvalidInput(_))));
    let neither = EditTool.call(json!({"path": "l.txt"}), &ctx).await;
    assert!(matches!(neither, Err(ToolError::InvalidInput(_))));
}

#[tokio::test]
async fn malformed_ops_are_invalid_input() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_in(tmp.path());
    tokio::fs::write(tmp.path().join("m.txt"), "a\n")
        .await
        .unwrap();
    for ops in [
        json!([{"op": "replace", "start": "garbage", "lines": []}]),
        json!([{"op": "explode", "start": "1#00"}]),
        json!([{"op": "insert_after", "at": "EOF", "lines": ["a\u{0}b"]}]),
    ] {
        let r = EditTool
            .call(json!({"path": "m.txt", "ops": ops}), &ctx)
            .await;
        assert!(matches!(r, Err(ToolError::InvalidInput(_))), "{r:?}");
    }
}

#[test]
fn schema_prefers_ops_and_keeps_fallback() {
    let schema = EditTool.schema();
    assert_eq!(schema["required"], json!(["path"]));
    assert!(schema["properties"]["ops"]["items"]["properties"]["lines"].is_object());
    assert!(schema["properties"]["old_string"].is_object());
    assert!(EditTool.description().contains("LINE#HASH"));
}
