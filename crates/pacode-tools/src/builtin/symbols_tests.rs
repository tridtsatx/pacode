//! `symbols` tool tests: outline/context modes, skip ledger, filters, caps.
use std::sync::Arc;

use serde_json::json;

use super::{NAME, SymbolsTool};
use crate::test_support::{StubToolHost, stub_ctx};
use crate::{Tool, ToolCtx, ToolError};

fn ctx(dir: &std::path::Path) -> ToolCtx {
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host);
    ctx.cwd = dir.to_path_buf();
    ctx
}

async fn call(dir: &std::path::Path, input: serde_json::Value) -> Result<String, ToolError> {
    SymbolsTool
        .call(input, &ctx(dir))
        .await
        .map(|out| out.content)
}

/// Symbol rows render `{kind:<8}{name}` — assert on the padded column pair.
fn has_row(out: &str, kind: &str, name: &str) -> bool {
    out.contains(&format!("{kind:<8}{name}"))
}

#[test]
fn name_and_kind() {
    assert_eq!(SymbolsTool.name(), "symbols");
    assert_eq!(NAME, "symbols");
    assert_eq!(SymbolsTool.kind(), crate::ToolKind::ReadOnly);
}

#[tokio::test]
async fn single_file_outline() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "struct Config {\n    a: u8,\n}\n\nimpl Config {\n    fn new() -> Self {\n        Self { a: 0 }\n    }\n}\n",
    )
    .unwrap();

    let out = call(tmp.path(), json!({"path": "lib.rs"})).await.unwrap();
    assert!(out.contains("lib.rs (rust)"), "{out}");
    assert!(has_row(&out, "struct", "Config"), "{out}");
    assert!(has_row(&out, "impl", "Config"), "{out}");
    assert!(has_row(&out, "method", "new"), "{out}");
    assert!(out.contains("1 file visited, 1 parsed"), "{out}");
    assert!(!out.contains("skipped"), "{out}");
}

#[tokio::test]
async fn context_mode_shows_body_lines() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("m.py"),
        "def long_fn():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    g = 7\n    return g\n",
    )
    .unwrap();

    let out = call(
        tmp.path(),
        json!({"path": "m.py", "mode": "context", "head_lines": 2, "tail_lines": 1}),
    )
    .await
    .unwrap();
    assert!(has_row(&out, "fn", "long_fn"), "{out}");
    assert!(out.contains("def long_fn():"), "{out}");
    assert!(out.contains("a = 1"), "{out}");
    assert!(out.contains("…"), "{out}");
    assert!(out.contains("return g"), "{out}");
    assert!(!out.contains("d = 4"), "tail/head elision failed: {out}");
}

#[tokio::test]
async fn directory_lists_skips() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "plain text\n").unwrap();
    std::fs::write(tmp.path().join("c.min.js"), "var x=1;\n").unwrap();
    // Binary content under a known extension exercises the NUL sniff;
    // an unknown extension would be `no grammar` before it is ever read.
    std::fs::write(tmp.path().join("d.py"), b"\x00\x9f\x92\x96".as_slice()).unwrap();

    let out = call(tmp.path(), json!({"path": "."})).await.unwrap();
    assert!(out.contains("a.rs (rust)"), "{out}");
    assert!(out.contains("skipped: 3"), "{out}");
    assert!(out.contains("no grammar"), "{out}");
    assert!(out.contains("generated"), "{out}");
    assert!(out.contains("binary"), "{out}");
    assert!(out.contains("b.txt"), "{out}");
    assert!(out.contains("c.min.js"), "{out}");
    assert!(out.contains("d.py"), "{out}");
    assert!(out.contains("4 files visited, 1 parsed"), "{out}");
}

#[tokio::test]
async fn glob_limits_directory_scan() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(tmp.path().join("src/b.py"), "def b():\n    pass\n").unwrap();

    let out = call(tmp.path(), json!({"path": ".", "glob": "**/*.py"}))
        .await
        .unwrap();
    assert!(out.contains("src/b.py (python)"), "{out}");
    assert!(!out.contains("a.rs"), "{out}");
    assert!(out.contains("1 file visited, 1 parsed"), "{out}");
}

#[tokio::test]
async fn kinds_and_depth_filters() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "struct S {}\nimpl S {\n    fn m(&self) {}\n}\nfn free() {}\n",
    )
    .unwrap();

    let out = call(tmp.path(), json!({"path": "lib.rs", "kinds": ["fn"]}))
        .await
        .unwrap();
    assert!(has_row(&out, "fn", "free"), "{out}");
    assert!(!out.contains("struct"), "{out}");
    assert!(!out.contains("method"), "{out}");

    let out = call(tmp.path(), json!({"path": "lib.rs", "depth": 0}))
        .await
        .unwrap();
    assert!(has_row(&out, "struct", "S"), "{out}");
    assert!(has_row(&out, "impl", "S"), "{out}");
    assert!(!out.contains("method"), "{out}");
}

#[tokio::test]
async fn lang_override_and_dir_lang_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("odd.txt"), "fn forced() {}\n").unwrap();

    let out = call(tmp.path(), json!({"path": "odd.txt", "lang": "rust"}))
        .await
        .unwrap();
    assert!(out.contains("odd.txt (rust)"), "{out}");
    assert!(has_row(&out, "fn", "forced"), "{out}");

    let err = call(tmp.path(), json!({"path": ".", "lang": "rust"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn parse_error_is_flagged_not_empty() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.rs"), "fn ok() {}\nfn broken( {\n").unwrap();

    let out = call(tmp.path(), json!({"path": "bad.rs"})).await.unwrap();
    assert!(
        out.contains("parsed with errors (first at line 2)"),
        "{out}"
    );
    assert!(has_row(&out, "fn", "ok"), "{out}");
}

#[tokio::test]
async fn max_symbols_truncates() {
    let tmp = tempfile::tempdir().unwrap();
    let mut src = String::new();
    for i in 0..20 {
        src.push_str(&format!("fn f{i}() {{}}\n"));
    }
    std::fs::write(tmp.path().join("many.rs"), src).unwrap();

    let out = call(tmp.path(), json!({"path": "many.rs", "max_symbols": 5}))
        .await
        .unwrap();
    assert!(out.contains("5 symbols shown"), "{out}");
    assert!(out.contains("max_symbols hit"), "{out}");
}

#[tokio::test]
async fn missing_path_and_bad_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let err = call(tmp.path(), json!({"path": "nope.rs"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");

    std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
    let err = call(tmp.path(), json!({"path": "a.rs", "kinds": ["nope"]}))
        .await
        .unwrap_err();
    match err {
        ToolError::InvalidInput(msg) => assert!(msg.contains("nope"), "{msg}"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let out = call(tmp.path(), json!({"path": "."})).await.unwrap();
    assert!(out.contains("no files matched"), "{out}");
}
