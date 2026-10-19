use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::ToolCtx;
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
        .call(
            json!({"path": "f.txt", "offset": 3, "limit": 4, "anchors": false}),
            &ctx,
        )
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
        .call(
            json!({"path": "f.txt", "offset": 7, "anchors": false}),
            &ctx,
        )
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

#[tokio::test]
async fn a_file_without_a_trailing_newline_still_yields_its_last_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("f.txt"), "a\nb")
        .await
        .expect("write");
    let ctx = ctx_in(tmp.path());

    let out = ReadTool
        .call(json!({"path": "f.txt", "anchors": false}), &ctx)
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

    let out = ReadTool
        .call(json!({"path": "crlf.txt", "anchors": false}), &ctx)
        .await
        .expect("read");
    assert!(out.content.contains("     1\ta\n"), "{:?}", out.content);
    assert!(!out.content.contains('\r'), "{:?}", out.content);

    let out = ReadTool
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
    let count = MAX_READ_BYTES as usize / 81 + 512;
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
    let out = ReadTool
        .call(
            json!({"path": "huge.txt", "accept_large_output": true, "anchors": false}),
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

    let capped = ReadTool
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
        ReadTool.call(json!({"path": "pipe"}), &ctx),
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
    ReadTool
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
    ReadTool
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

    let err = ReadTool
        .call(json!({"path": ".env"}), &ctx)
        .await
        .expect_err("a denied secret read is refused");
    assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
}

/// A text file of `lines` lines, `line N` on line N (1-based).
fn numbered_text(lines: usize) -> String {
    (1..=lines).map(|i| format!("line {i}\n")).collect()
}

async fn setup(name: &str, bytes: &[u8]) -> (tempfile::TempDir, ToolCtx, Arc<StubToolHost>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join(name), bytes)
        .await
        .expect("write");
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host.clone());
    ctx.cwd = tmp.path().to_path_buf();
    ctx.output_cap_chars = 1_000_000;
    (tmp, ctx, host)
}

/// Just over the cap: numbered `line N` rows, so a ranged read can be checked by content.
fn over_cap_text() -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(MAX_READ_BYTES as usize + 64 * 1024);
    let mut n = 1usize;
    while text.len() as u64 <= MAX_READ_BYTES + 1024 {
        let _ = writeln!(text, "line {n}");
        n += 1;
    }
    debug_assert!(n > 1000);
    text
}

#[tokio::test]
async fn large_text_without_range_shows_the_head_with_a_hint() {
    let (_tmp, ctx, _host) = setup("big.txt", over_cap_text().as_bytes()).await;
    let out = ReadTool
        .call(json!({"path": "big.txt"}), &ctx)
        .await
        .expect("a file over the cap shows its first lines");
    let msg = out.content;
    assert!(msg.starts_with("1#"), "anchored from line 1");
    assert!(msg.contains("offset=2001"), "hint names offset: {msg}");
    assert!(msg.contains("grep"), "hint names grep: {msg}");
}

#[tokio::test]
async fn large_text_without_range_refuses_symbol() {
    let (_tmp, ctx, _host) = setup("big.txt", over_cap_text().as_bytes()).await;
    let err = ReadTool
        .call(json!({"path": "big.txt", "symbol": "main"}), &ctx)
        .await
        .expect_err("no outline of a file over the cap");
    assert!(err.to_string().contains("offset"), "{err}");
}

#[tokio::test]
async fn large_text_with_offset_and_limit_streams_only_the_range() {
    let (_tmp, ctx, _host) = setup("big.txt", over_cap_text().as_bytes()).await;
    let out = ReadTool
        .call(
            json!({"path": "big.txt", "offset": 1000, "limit": 10}),
            &ctx,
        )
        .await
        .expect("ranged read of a large file");
    let text = out.content;
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.len(), 10);
    assert!(
        rows[0].starts_with("1000#"),
        "anchored from offset: {}",
        rows[0]
    );
    assert!(rows[0].ends_with("| line 1000"), "row content: {}", rows[0]);
    assert!(rows[9].ends_with("| line 1009"), "row content: {}", rows[9]);
}

#[tokio::test]
async fn large_text_with_symbol_is_still_rejected_when_ranged() {
    let (_tmp, ctx, _host) = setup("big.txt", over_cap_text().as_bytes()).await;
    let err = ReadTool
        .call(
            json!({"path": "big.txt", "offset": 1, "symbol": "main"}),
            &ctx,
        )
        .await
        .expect_err("symbol cannot combine with offset");
    assert!(err.to_string().contains("cannot be combined"), "{err}");
}

#[tokio::test]
async fn large_binary_keeps_the_binary_refusal() {
    let mut bytes = vec![b'a'; MAX_READ_BYTES as usize + 16];
    bytes[10] = 0;
    let (_tmp, ctx, _host) = setup("big.bin", &bytes).await;
    let err = ReadTool
        .call(json!({"path": "big.bin"}), &ctx)
        .await
        .expect_err("binary files are refused");
    assert!(err.to_string().contains("binary"), "{err}");
}

#[tokio::test]
async fn small_file_reads_are_unchanged() {
    let (_tmp, ctx, _host) = setup("small.txt", numbered_text(5).as_bytes()).await;
    let out = ReadTool
        .call(json!({"path": "small.txt", "offset": 2, "limit": 2}), &ctx)
        .await
        .expect("small ranged read");
    let text = out.content;
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.len(), 3);
    assert!(rows[0].starts_with("2#"), "{}", rows[0]);
    assert!(rows[1].ends_with("| line 3"), "{}", rows[1]);
    assert_eq!(rows[2], "— 2 more lines; read with offset=4");
}

#[test]
fn read_line_range_matches_str_lines_splitting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("crlf.txt");
    std::fs::write(&path, b"a\r\nb\n\nc\r").expect("write");
    let got = read_line_range(&path, 1, 10).expect("range");
    assert_eq!(got, vec!["a", "b", "", "c\r"]);
    let tail = read_line_range(&path, 3, 1).expect("range");
    assert_eq!(tail, vec![""]);
}
