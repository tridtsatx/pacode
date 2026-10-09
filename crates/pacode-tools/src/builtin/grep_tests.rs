use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

fn ctx_in(dir: &std::path::Path) -> crate::ToolCtx {
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = dir.to_path_buf();
    ctx
}

#[tokio::test]
async fn hitting_max_results_says_the_list_may_be_partial() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a.txt"), "hit 1\nhit 2\nhit 3\n").expect("write");
    let ctx = ctx_in(tmp.path());

    let out = GrepTool::default()
        .call(json!({"pattern": "hit", "max_results": 2}), &ctx)
        .await
        .expect("grep");
    assert!(out.content.contains("a.txt:1:hit 1"));
    assert!(out.content.contains("a.txt:2:hit 2"));
    assert!(!out.content.contains("hit 3"));
    assert!(
        out.content
            .ends_with("— stopped at max_results=2; more matches may exist, narrow `path`/`glob` or raise `max_results`"),
        "content: {}",
        out.content
    );

    // Under the limit: the list is complete and carries no footer.
    let out = GrepTool::default()
        .call(json!({"pattern": "hit"}), &ctx)
        .await
        .expect("grep");
    assert_eq!(out.content, "a.txt:1:hit 1\na.txt:2:hit 2\na.txt:3:hit 3");
}

#[tokio::test]
async fn a_huge_line_is_clipped_around_the_match() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let line = format!("{}NEEDLE{}", "a".repeat(5_000), "b".repeat(5_000));
    std::fs::write(tmp.path().join("min.js"), format!("{line}\nshort NEEDLE\n")).expect("write");
    let ctx = ctx_in(tmp.path());

    let out = GrepTool::default()
        .call(json!({"pattern": "NEEDLE", "path": "min.js"}), &ctx)
        .await
        .expect("grep");
    let rows: Vec<&str> = out.content.lines().collect();
    assert_eq!(rows.len(), 2, "content: {}", out.content);
    let clipped = rows[0];
    assert!(clipped.starts_with("min.js:1:…"), "row: {clipped}");
    assert!(
        clipped.contains("NEEDLE"),
        "the match stays visible: {clipped}"
    );
    assert!(
        clipped.ends_with("… [line is 10006 bytes]"),
        "row: {clipped}"
    );
    assert!(
        clipped.len() < MAX_LINE_BYTES + 64,
        "row is bounded: {}",
        clipped.len()
    );
    // A short line is untouched.
    assert_eq!(rows[1], "min.js:2:short NEEDLE");
}

#[test]
fn clipping_respects_char_boundaries_and_line_start() {
    use grep_regex::RegexMatcher;
    let matcher = RegexMatcher::new("zzz").expect("regex");
    // Multi-byte chars everywhere, no match: the window keeps the line start.
    let line = "é".repeat(1_000);
    let clipped = clip_line(&line, &matcher);
    assert!(clipped.starts_with('é'));
    assert!(clipped.ends_with("… [line is 2000 bytes]"));
    // Short line borrowed as is.
    assert!(matches!(
        clip_line("short", &matcher),
        Cow::Borrowed("short")
    ));
}
