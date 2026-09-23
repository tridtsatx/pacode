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
async fn a_limit_cut_listing_ends_with_the_continuation_offset() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    tokio::fs::write(tmp.path().join("f.txt"), text)
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    let out = ReadTool
        .call(json!({"path": "f.txt", "offset": 3, "limit": 4}), &ctx)
        .await
        .expect("read");

    assert!(out.content.contains("     6\tline 6"));
    assert!(!out.content.contains("line 7"));
    assert!(
        out.content.contains("— 4 more lines; read with offset=7"),
        "content: {}",
        out.content
    );

    // Reading to the end of the file appends nothing.
    let out = ReadTool
        .call(json!({"path": "f.txt", "offset": 7}), &ctx)
        .await
        .expect("read tail");
    assert!(out.content.contains("    10\tline 10"));
    assert!(
        !out.content.contains("more line"),
        "content: {}",
        out.content
    );

    // An offset past the end has nothing to continue into either.
    let out = ReadTool
        .call(json!({"path": "f.txt", "offset": 50}), &ctx)
        .await
        .expect("read past end");
    assert!(
        !out.content.contains("more line"),
        "content: {}",
        out.content
    );
}

#[tokio::test]
async fn a_single_remaining_line_is_singular() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("f.txt"), "a\nb\n")
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    let out = ReadTool
        .call(json!({"path": "f.txt", "limit": 1}), &ctx)
        .await
        .expect("read");
    assert!(
        out.content.contains("— 1 more line; read with offset=2"),
        "content: {}",
        out.content
    );
}
