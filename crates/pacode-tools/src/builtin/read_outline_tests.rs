use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::hashline::line_hash;
use crate::test_support::{StubToolHost, stub_ctx};

/// 512 lines: a `use`, then 30 functions of 17 lines each (doc, signature, 13 body
/// lines, `}`, blank). The outline keeps doc + signature + 3 body lines + `}` + blank
/// of each and hides 10 body lines per function: 212 lines shown.
fn big_rust() -> String {
    let mut s = String::from("use std::fmt;\n\n");
    for i in 0..30 {
        s.push_str(&format!("/// Doc {i}.\npub fn f{i}(a: u32) -> u32 {{\n"));
        for j in 0..12 {
            s.push_str(&format!("    let v{j} = a + {j};\n"));
        }
        s.push_str("    a\n}\n\n");
    }
    s
}

async fn setup(name: &str, content: &str) -> (tempfile::TempDir, ToolCtx, Arc<StubToolHost>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join(name), content)
        .await
        .expect("write");
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host.clone());
    ctx.cwd = tmp.path().to_path_buf();
    ctx.output_cap_chars = 1_000_000;
    (tmp, ctx, host)
}

async fn read(ctx: &ToolCtx, input: Value) -> Result<ToolOutput, ToolError> {
    ReadTool.call(input, ctx).await
}

/// The ledger reads a call recorded (the stub also logs `lock`).
fn reads(host: &StubToolHost) -> Vec<String> {
    host.ledger
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("read "))
        .collect()
}

fn is_anchored(row: &str) -> bool {
    row.split_once('#')
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[tokio::test]
async fn big_file_reads_as_outline_with_anchors_only_on_shown_lines() {
    let src = big_rust();
    let (_tmp, ctx, host) = setup("big.rs", &src).await;
    let out = read(&ctx, json!({"path": "big.rs"})).await.expect("read");
    let rows: Vec<&str> = out.content.lines().collect();
    assert_eq!(
        rows[0],
        "outline of big.rs (512 lines, 212 shown); use symbol:\"name\" or expand:[\"a-b\"] \
         to see hidden code; mode:\"full\" for everything"
    );
    let lines: Vec<&str> = src.lines().collect();
    let mut anchored = Vec::new();
    for row in &rows[1..] {
        if is_anchored(row) {
            let (n, _) = row.split_once('#').expect("anchor");
            let n: usize = n.parse().expect("line number");
            let text = lines[n - 1];
            assert_eq!(*row, format!("{n}#{}| {text}", line_hash(text)));
            anchored.push(n);
        } else {
            assert!(row.trim_start().starts_with("… "), "unexpected row {row:?}");
        }
    }
    assert_eq!(anchored.len(), 212);
    // fn f0's body lines 8..=17 are hidden: no anchors for them.
    assert!(anchored.iter().all(|n| !(8..=17).contains(n)));
    assert_eq!(rows[8], "    … 10 lines hidden (L8-L17): fn f0 …");
    // Only shown lines are reported to the ledger: 31 kept ranges.
    let calls = reads(&host);
    assert_eq!(calls.len(), 31);
    assert_eq!(calls[0], "read big.rs 0..7");
    assert_eq!(calls[1], "read big.rs 17..24");
    assert_eq!(calls[30], "read big.rs 510..512");
}

#[tokio::test]
async fn small_file_and_explicit_modes() {
    let (_tmp, ctx, _host) = setup("small.rs", "fn a() {\n    1\n}\n").await;
    let plain = read(&ctx, json!({"path": "small.rs"})).await.expect("read");
    assert!(plain.content.starts_with("1#"), "{}", plain.content);
    let forced = read(&ctx, json!({"path": "small.rs", "mode": "outline"}))
        .await
        .expect("read");
    assert!(
        forced
            .content
            .starts_with("outline of small.rs (3 lines, 3 shown)")
    );

    let src = big_rust();
    let (_tmp, ctx, _host) = setup("big.rs", &src).await;
    let full = read(&ctx, json!({"path": "big.rs", "mode": "full"}))
        .await
        .expect("read");
    assert_eq!(full.content.lines().count(), 512);
    let ranged = read(&ctx, json!({"path": "big.rs", "offset": 1, "limit": 600}))
        .await
        .expect("read");
    assert_eq!(ranged.content.lines().count(), 512);
    let unanchored = read(&ctx, json!({"path": "big.rs", "anchors": false}))
        .await
        .expect("read");
    assert_eq!(unanchored.content.lines().count(), 512);
}

#[tokio::test]
async fn symbol_returns_the_whole_declaration() {
    let src = big_rust();
    let (_tmp, ctx, host) = setup("big.rs", &src).await;
    let out = read(&ctx, json!({"path": "big.rs", "symbol": "f3"}))
        .await
        .expect("read");
    let rows: Vec<&str> = out.content.lines().collect();
    assert_eq!(rows[0], "fn f3 in big.rs (L54-L69):");
    assert_eq!(rows.len(), 1 + 16);
    assert!(rows[1..].iter().all(|r| is_anchored(r)));
    assert_eq!(rows[16], format!("69#{}| }}", line_hash("}")));
    assert_eq!(reads(&host), ["read big.rs 53..69"]);
}

#[tokio::test]
async fn unknown_symbol_lists_what_exists() {
    let (_tmp, ctx, _host) = setup("big.rs", &big_rust()).await;
    let err = read(&ctx, json!({"path": "big.rs", "symbol": "nope"}))
        .await
        .expect_err("missing symbol");
    let msg = err.to_string();
    assert!(
        msg.contains("no symbol \"nope\" in big.rs; symbols: f0, f1"),
        "{msg}"
    );
}

#[tokio::test]
async fn expand_returns_ranges_with_anchors() {
    let src = big_rust();
    let (_tmp, ctx, host) = setup("big.rs", &src).await;
    let out = read(&ctx, json!({"path": "big.rs", "expand": ["8-17", "L511"]}))
        .await
        .expect("read");
    let rows: Vec<&str> = out.content.lines().collect();
    assert_eq!(rows[0], "big.rs (L8-L17):");
    assert_eq!(
        rows[1],
        format!(
            "8#{}|     let v3 = a + 3;",
            line_hash("    let v3 = a + 3;")
        )
    );
    assert_eq!(rows[12], "big.rs (L511-L511):");
    assert_eq!(reads(&host), ["read big.rs 7..17", "read big.rs 510..511"]);
}

#[tokio::test]
async fn bad_combinations_and_ranges_are_rejected() {
    let (_tmp, ctx, _host) = setup("big.rs", &big_rust()).await;
    let err = read(&ctx, json!({"path": "big.rs", "offset": 3, "symbol": "f1"}))
        .await
        .expect_err("offset + symbol");
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");
    let err = read(&ctx, json!({"path": "big.rs", "expand": ["9000-9001"]}))
        .await
        .expect_err("past the end");
    assert!(err.to_string().contains("starts past the end"), "{err}");
}

#[tokio::test]
async fn unsupported_or_broken_files_read_plainly() {
    let text: String = (0..600).map(|i| format!("line {i}\n")).collect();
    let (_tmp, ctx, _host) = setup("notes.txt", &text).await;
    let out = read(&ctx, json!({"path": "notes.txt"}))
        .await
        .expect("read");
    assert!(out.content.starts_with("1#"));
    let err = read(&ctx, json!({"path": "notes.txt", "mode": "outline"}))
        .await
        .expect_err("no outline for txt");
    assert!(err.to_string().contains("mode:\"full\""), "{err}");

    let garbage = "fn ( { [ @@ ## $$ %% ^^ && ** \n".repeat(600);
    let (_tmp, ctx, _host) = setup("bad.rs", &garbage).await;
    let out = read(&ctx, json!({"path": "bad.rs"})).await.expect("read");
    assert!(
        out.content.starts_with("1#"),
        "syntax errors fall back to a plain read"
    );
}

#[tokio::test]
async fn crlf_file_outlines_with_the_same_anchors() {
    let src = big_rust().replace('\n', "\r\n");
    let (_tmp, ctx, _host) = setup("crlf.rs", &src).await;
    let out = read(&ctx, json!({"path": "crlf.rs"})).await.expect("read");
    let rows: Vec<&str> = out.content.lines().collect();
    assert!(rows[0].starts_with("outline of crlf.rs (512 lines, 212 shown)"));
    let sig = "pub fn f0(a: u32) -> u32 {";
    assert_eq!(rows[4], format!("4#{}| {sig}", line_hash(sig)));
}

#[tokio::test]
async fn configured_threshold_applies() {
    let src = big_rust();
    let (_tmp, ctx, _host) = setup("big.rs", &src).await;
    let never = ConfiguredRead::new(ReadConfig {
        outline_threshold: 0,
    });
    let out = never
        .call(json!({"path": "big.rs"}), &ctx)
        .await
        .expect("read");
    assert_eq!(out.content.lines().count(), 512);
    let high = ConfiguredRead::new(ReadConfig {
        outline_threshold: 600,
    });
    let out = high
        .call(json!({"path": "big.rs"}), &ctx)
        .await
        .expect("read");
    assert_eq!(out.content.lines().count(), 512);
    let low = ConfiguredRead::new(ReadConfig {
        outline_threshold: 100,
    });
    let out = low
        .call(json!({"path": "big.rs"}), &ctx)
        .await
        .expect("read");
    assert!(out.content.starts_with("outline of big.rs"));
    assert_eq!(low.name(), "read");
}

#[test]
fn parse_range_accepts_the_marker_forms() {
    assert_eq!(parse_range("44-80", 100).expect("range"), 43..80);
    assert_eq!(parse_range("L44-L80", 100).expect("range"), 43..80);
    assert_eq!(parse_range(" 7 ", 100).expect("range"), 6..7);
    assert_eq!(parse_range("90-200", 100).expect("range"), 89..100);
    for bad in ["0-3", "5-4", "a-b", "", "3-"] {
        assert!(parse_range(bad, 100).is_err(), "{bad}");
    }
    assert!(parse_range("101", 100).is_err());
}

#[test]
fn marker_copies_indentation_and_names_the_region() {
    let lines = ["fn f() {", "", "\t\tlet a = 1;", "    b"];
    let hidden = Hidden {
        lines: 1..4,
        label: Some("fn f".to_string()),
    };
    assert_eq!(
        marker(&hidden, &lines),
        "\t\t… 3 lines hidden (L2-L4): fn f …"
    );
    let top = Hidden {
        lines: 0..1,
        label: None,
    };
    assert_eq!(marker(&top, &lines), "… 1 lines hidden (L1-L1) …");
}

#[test]
fn plain_length_matches_annotation() {
    let lines = ["ab", "", "xyz"];
    let rendered = (0..3)
        .map(|i| annotated_line(i + 1, lines[i]))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(plain_len(&lines), rendered.len() + 1);
}
