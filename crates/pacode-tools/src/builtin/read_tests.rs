use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

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
async fn large_text_without_range_is_refused_with_a_hint() {
    let (_tmp, ctx, _host) = setup("big.txt", over_cap_text().as_bytes()).await;
    let err = ReadTool
        .call(json!({"path": "big.txt"}), &ctx)
        .await
        .expect_err("a file over the cap needs offset/limit");
    let msg = err.to_string();
    assert!(msg.contains("offset"), "hint names offset: {msg}");
    assert!(msg.contains("grep"), "hint names grep: {msg}");
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
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("2#"), "{}", rows[0]);
    assert!(rows[1].ends_with("| line 3"), "{}", rows[1]);
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
