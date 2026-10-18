use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

fn ctx_in(dir: &tempfile::TempDir) -> ToolCtx {
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = dir.path().to_path_buf();
    ctx
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "x\ny\n").expect("write");
    std::fs::write(dir.path().join("b.rs"), "z\n").expect("write");
    std::fs::write(
        dir.path().join("data.json"),
        "{\"items\":[{\"n\":\"b\"},{\"n\":\"a\"}]}",
    )
    .expect("write");
    dir
}

#[tokio::test]
async fn a_pipeline_with_a_glob_runs_in_process() {
    let dir = fixture();
    let out = UtilTool
        .call(
            json!({"cmd": "wc -l *.rs | sort -n", "intent": "count"}),
            &ctx_in(&dir),
        )
        .await
        .expect("util");
    assert_eq!(out.content, "1 b.rs\n2 a.rs\n3 total\n");
    assert!(!out.is_error);
    assert!(out.title.starts_with("Util wc -l"), "{}", out.title);
}

#[tokio::test]
async fn jq_and_sort_compose() {
    let dir = fixture();
    let out = UtilTool
        .call(
            json!({"cmd": "jq -r '.items[].n' data.json | sort", "intent": "names"}),
            &ctx_in(&dir),
        )
        .await
        .expect("util");
    assert_eq!(out.content, "a\nb\n");
}

#[tokio::test]
async fn failures_are_marked_and_show_stderr_and_exit_code() {
    let dir = fixture();
    let out = UtilTool
        .call(
            json!({"cmd": "wc -l nope.txt", "intent": "x"}),
            &ctx_in(&dir),
        )
        .await
        .expect("util");
    assert!(out.is_error);
    assert!(out.content.contains("No such file"), "{}", out.content);
    assert!(out.content.ends_with("[exit code 1]"), "{}", out.content);
}

#[tokio::test]
async fn diff_differences_are_not_an_error() {
    let dir = fixture();
    let out = UtilTool
        .call(
            json!({"cmd": "diff a.rs b.rs", "intent": "x"}),
            &ctx_in(&dir),
        )
        .await
        .expect("util");
    assert!(!out.is_error);
    assert!(out.content.contains("--- a.rs"), "{}", out.content);
}

#[tokio::test]
async fn unsupported_syntax_and_utilities_are_refused() {
    let dir = fixture();
    for cmd in [
        "wc a.rs > out",
        "ls",
        "wc $HOME",
        "wc a.rs; wc b.rs",
        "",
        "wc *.zzz",
    ] {
        let err = UtilTool
            .call(json!({"cmd": cmd, "intent": "x"}), &ctx_in(&dir))
            .await
            .expect_err(cmd);
        assert!(matches!(err, ToolError::InvalidInput(_)), "{cmd}: {err}");
    }
}

#[tokio::test]
async fn output_is_capped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big: String = (0..5000).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &big).expect("write");
    let mut ctx = ctx_in(&dir);
    ctx.output_cap_chars = 500;
    let out = UtilTool
        .call(json!({"cmd": "tac big.txt", "intent": "x"}), &ctx)
        .await
        .expect("util");
    assert!(out.content.chars().count() < 2000, "{}", out.content.len());
}

#[test]
fn the_tool_is_read_only_and_advertises_its_utilities() {
    assert_eq!(UtilTool.kind(), ToolKind::ReadOnly);
    let description = UtilTool.description();
    for name in ["wc", "sort", "jq", "sed", "diff", "tree", "sha256sum"] {
        assert!(description.contains(name), "{name}");
    }
    assert!(description.contains("Prefer this"));
    assert_eq!(UtilTool.schema()["required"][0], "cmd");
}

#[test]
fn outside_workspace_paths_are_detected() {
    let dir = fixture();
    let stages = pipeline::parse("wc -l a.rs /etc/hostname", dir.path(), false).expect("parse");
    assert_eq!(outside_paths(&stages, dir.path()), vec!["/etc/hostname"]);
    let stages = pipeline::parse("sed -n '/x/p' a.rs", dir.path(), false).expect("parse");
    assert!(outside_paths(&stages, dir.path()).is_empty());
}

#[tokio::test]
async fn bash_fast_path_only_takes_clean_pipelines() {
    let dir = fixture();
    let took = try_in_process("wc -l a.rs | sort -n", dir.path()).await;
    let out = took.expect("pure pipeline runs in-process");
    assert_eq!(out.stdout, b"2 a.rs\n");

    assert!(
        try_in_process("wc -l *.rs", dir.path()).await.is_none(),
        "globs go to the shell"
    );
    assert!(try_in_process("wc -l a.rs > o", dir.path()).await.is_none());
    assert!(try_in_process("cargo test", dir.path()).await.is_none());
    assert!(
        try_in_process("wc -l missing", dir.path()).await.is_none(),
        "failures re-run for real"
    );
    assert!(
        try_in_process("sort -z a.rs", dir.path()).await.is_none(),
        "unsupported flags fall back"
    );
    assert!(
        try_in_process("sort", dir.path()).await.is_none(),
        "no input falls back"
    );

    let diff = try_in_process("diff a.rs b.rs", dir.path())
        .await
        .expect("diff");
    assert_eq!(diff.code, 1);
    let (content, preview) = render_for_bash(&diff, false, 16_000);
    assert!(content.ends_with("[exit code 1]"), "{content}");
    assert!(!preview.is_empty());
}
