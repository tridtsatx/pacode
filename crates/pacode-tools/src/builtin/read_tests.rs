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

    let out = ReadTool::default()
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
    let out = ReadTool::default()
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
    let out = ReadTool::default()
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

    let out = ReadTool::default()
        .call(json!({"path": "f.txt", "limit": 1}), &ctx)
        .await
        .expect("read");
    assert!(
        out.content.contains("— 1 more line; read with offset=2"),
        "content: {}",
        out.content
    );
}

#[tokio::test]
async fn a_file_without_a_trailing_newline_still_yields_its_last_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("f.txt"), "a\nb")
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    let out = ReadTool::default()
        .call(json!({"path": "f.txt"}), &ctx)
        .await
        .expect("read");

    assert!(out.content.contains("     1\ta"), "{}", out.content);
    assert!(out.content.contains("     2\tb"), "{}", out.content);
    assert!(
        !out.content.contains("more line"),
        "content: {}",
        out.content
    );
}

#[tokio::test]
async fn carriage_returns_are_stripped_and_empty_files_read_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("crlf.txt"), "a\r\nb\r\n")
        .await
        .expect("write");
    tokio::fs::write(tmp.path().join("empty.txt"), "")
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    let out = ReadTool::default()
        .call(json!({"path": "crlf.txt"}), &ctx)
        .await
        .expect("read");
    assert!(out.content.contains("     1\ta\n"), "{:?}", out.content);
    assert!(!out.content.contains('\r'), "{:?}", out.content);

    let out = ReadTool::default()
        .call(json!({"path": "empty.txt"}), &ctx)
        .await
        .expect("read");
    assert_eq!(out.content, "");
}

#[tokio::test]
async fn a_scan_cut_by_the_cap_says_so_and_never_shows_half_a_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // 81-byte lines, so a line straddles the 64 KiB chunk and the cap lands
    // mid-line rather than on a boundary.
    let line = format!("{}\n", "x".repeat(80));
    let count = MAX_READ_SCAN_BYTES as usize / 81 + 512;
    let mut filler = String::with_capacity(count * 81);
    for _ in 0..count {
        filler.push_str(&line);
    }
    tokio::fs::write(tmp.path().join("huge.txt"), filler.as_bytes())
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    // `accept_large_output` skips the char cap, so what is left is exactly what
    // the scan produced: a leaked fragment here would be a half-read line.
    let out = ReadTool::default()
        .call(
            json!({"path": "huge.txt", "accept_large_output": true}),
            &ctx,
        )
        .await
        .expect("read");

    assert!(
        out.content.contains("byte read cap"),
        "content tail: {}",
        &out.content[out.content.len().saturating_sub(200)..]
    );
    assert!(out.content.contains("     1\t"), "first line must be there");
    // The window is the first 2000 lines; nothing past the scan cap leaks in.
    assert!(
        out.content.contains("  2000\t"),
        "listing stops at the line cap"
    );
    let listing = out.content.split("— ").next().unwrap_or_default();
    let shown = listing.lines().count();
    assert_eq!(
        shown, 2000,
        "every listed line must be a whole numbered one"
    );
    for entry in listing.lines() {
        assert!(
            entry.contains('\t'),
            "a cut line leaked into the output: {entry:?}"
        );
    }

    let capped = ReadTool::default()
        .call(json!({"path": "huge.txt"}), &ctx)
        .await
        .expect("read");
    assert!(
        capped.content.contains("byte read cap"),
        "the cap note must survive the char cap"
    );
}

#[tokio::test]
async fn a_non_regular_file_is_refused_before_it_can_block() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fifo = tmp.path().join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !made {
        return;
    }
    let ctx = ctx_in(tmp.path());

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ReadTool::default().call(json!({"path": "pipe"}), &ctx),
    )
    .await
    .expect("read must not hang on a fifo")
    .expect_err("a fifo is not readable");

    assert!(format!("{err}").contains("not a regular file"), "{err}");
}

#[tokio::test]
async fn reading_a_secret_file_asks_with_exec_confirm() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join(".env"), "TOKEN=abc\n")
        .await
        .expect("write");
    tokio::fs::write(tmp.path().join("notes.txt"), "hi\n")
        .await
        .expect("write");
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host.clone());
    ctx.cwd = tmp.path().to_path_buf();

    // A secret read records an Exec/Confirm permission draft before reading.
    ReadTool::default()
        .call(json!({"path": ".env"}), &ctx)
        .await
        .expect("read .env");
    {
        let drafts = host.drafts.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(drafts.len(), 1, "a secret read must ask once");
        assert_eq!(drafts[0].tool_kind, Some(ToolKind::Exec));
        assert_eq!(drafts[0].risk, Some(pacode_risk::RiskLevel::Confirm));
    }

    // An ordinary file is read without a prompt.
    ReadTool::default()
        .call(json!({"path": "notes.txt"}), &ctx)
        .await
        .expect("read notes.txt");
    let drafts_len = host.drafts.lock().unwrap_or_else(|p| p.into_inner()).len();
    assert_eq!(drafts_len, 1, "an ordinary read must not ask");
}

#[tokio::test]
async fn a_denied_secret_read_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join(".env"), "TOKEN=abc\n")
        .await
        .expect("write");
    let host = Arc::new(StubToolHost::default());
    *host.decision.lock().unwrap_or_else(|p| p.into_inner()) =
        pacode_types::state::PermissionDecision::Deny;
    let mut ctx = stub_ctx(host.clone());
    ctx.cwd = tmp.path().to_path_buf();

    let err = ReadTool::default()
        .call(json!({"path": ".env"}), &ctx)
        .await
        .expect_err("a denied secret read is refused");
    assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
}
