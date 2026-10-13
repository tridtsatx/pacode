use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_symbols::{FileSymbols, Language};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::SymbolsTool;
use super::{ParseAhead, WAVE, parse_file};
use crate::Tool;
use crate::test_support::{StubToolHost, stub_ctx};

const FILES: usize = 70;

/// `FILES` Rust files, more than two waves, with `fns` functions each, and a
/// grammar-less text file after every tenth so cheap skips sit between waves.
fn tree(fns: usize) -> (tempfile::TempDir, Vec<PathBuf>) {
    const { assert!(FILES > 2 * WAVE) };
    let tmp = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for i in 0..FILES {
        let body: String = (0..fns).map(|f| format!("fn f{i}_{f}() {{}}\n")).collect();
        let path = tmp.path().join(format!("f{i:02}.rs"));
        std::fs::write(&path, body).unwrap();
        files.push(path);
        if i % 10 == 0 {
            std::fs::write(tmp.path().join(format!("f{i:02}.txt")), "text\n").unwrap();
        }
    }
    (tmp, files)
}

fn names(symbols: &FileSymbols) -> Vec<String> {
    symbols.symbols.iter().map(|s| s.name.clone()).collect()
}

async fn symbols(dir: &Path, input: serde_json::Value) -> String {
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = dir.to_path_buf();
    SymbolsTool::default()
        .call(input, &ctx)
        .await
        .unwrap()
        .content
}

#[test]
fn waves_hand_back_what_a_serial_parse_gives_in_path_order() {
    let (_tmp, files) = tree(2);
    let queue = files.iter().map(|p| (p.clone(), Language::Rust)).collect();
    let mut ahead = ParseAhead::new(queue, true);
    let cancel = CancellationToken::new();

    for path in &files {
        let (source, outline) = ahead.next(usize::MAX, &cancel).unwrap().unwrap();
        let (want_source, want) = parse_file(path, Language::Rust).unwrap();
        assert_eq!(source, want_source, "{path:?}");
        assert_eq!(names(&outline), names(&want), "{path:?}");
    }
}

#[test]
fn outline_mode_does_not_hold_the_sources() {
    let (_tmp, files) = tree(1);
    let queue = files.iter().map(|p| (p.clone(), Language::Rust)).collect();
    let mut ahead = ParseAhead::new(queue, false);
    let (source, outline) = ahead
        .next(usize::MAX, &CancellationToken::new())
        .unwrap()
        .unwrap();
    assert!(source.is_empty());
    assert_eq!(names(&outline), ["f0_0"]);
}

/// Three rows per file and `max_symbols` 100: a serial scan parses 34 files
/// (34 × 3 = 102 ≥ 100) and stops before the 35th, mid-wave.
#[tokio::test]
async fn max_symbols_stops_on_the_same_file_as_a_serial_scan() {
    let (tmp, _) = tree(3);
    let out = symbols(tmp.path(), json!({"path": ".", "max_symbols": 100})).await;
    assert!(out.contains("f33.rs (rust)"), "{out}");
    assert!(!out.contains("f34.rs (rust)"), "{out}");
    assert!(out.contains(", 34 parsed,"), "{out}");
    assert!(out.contains("max_symbols hit"), "{out}");
}

#[tokio::test]
async fn max_files_parses_exactly_that_many() {
    let (tmp, _) = tree(1);
    let out = symbols(tmp.path(), json!({"path": ".", "max_files": 40})).await;
    assert!(out.contains("f39.rs (rust)"), "{out}");
    assert!(!out.contains("f40.rs (rust)"), "{out}");
    assert!(out.contains(", 40 parsed,"), "{out}");
    assert!(out.contains("max_files hit"), "{out}");
    // The text files before the stop are skipped in path order.
    assert!(out.contains("skipped: 4 (4 no grammar)"), "{out}");
}
