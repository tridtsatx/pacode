use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

#[tokio::test]
async fn a_capped_listing_names_how_many_matches_it_hid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for name in ["a.rs", "b.rs", "c.rs", "d.txt"] {
        std::fs::write(tmp.path().join(name), "x").expect("write");
    }
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = tmp.path().to_path_buf();

    let out = GlobTool::default()
        .call(json!({"pattern": "*.rs", "max_results": 2}), &ctx)
        .await
        .expect("glob");
    let rows: Vec<&str> = out.content.lines().collect();
    assert_eq!(rows.len(), 3, "content: {}", out.content);
    assert!(rows[..2].iter().all(|r| r.ends_with(".rs")));
    assert_eq!(
        rows[2],
        "— 1 more matching files not shown (newest 2 listed); narrow the pattern or raise `max_results`"
    );

    // Everything fits: no footer.
    let out = GlobTool::default()
        .call(json!({"pattern": "*.rs"}), &ctx)
        .await
        .expect("glob");
    assert_eq!(out.content.lines().count(), 3);
    assert!(!out.content.contains('—'));
}

/// Matches come newest first by the mtime the walk records for each entry
/// (ties in path order, since the walk is sorted).
#[tokio::test]
async fn matches_are_listed_newest_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for (name, age_secs) in [("old.rs", 300), ("new.rs", 0), ("mid.rs", 100)] {
        let path = tmp.path().join(name);
        std::fs::write(&path, "x").expect("write");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open");
        file.set_modified(base - std::time::Duration::from_secs(age_secs))
            .expect("set mtime");
    }
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = tmp.path().to_path_buf();

    let out = GlobTool::default()
        .call(json!({"pattern": "*.rs"}), &ctx)
        .await
        .expect("glob");
    assert_eq!(out.content, "new.rs\nmid.rs\nold.rs");
}

/// 600 files whose mtime increases with their number: a bounded result is the newest N,
/// newest first, exactly what a full sort would give.
#[tokio::test]
async fn bounded_glob_returns_newest_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for i in 0..600u64 {
        let path = tmp.path().join(format!("f{i}.txt"));
        std::fs::write(&path, "x").expect("write");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open");
        file.set_modified(base + Duration::from_secs(i))
            .expect("set mtime");
    }
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = tmp.path().to_path_buf();
    ctx.output_cap_chars = 1_000_000;

    let out = GlobTool::default()
        .call(json!({"pattern": "*.txt", "max_results": 5}), &ctx)
        .await
        .expect("glob");
    let text = out.content;
    let names: Vec<&str> = text.lines().take(5).collect();
    assert_eq!(
        names,
        ["f599.txt", "f598.txt", "f597.txt", "f596.txt", "f595.txt"]
    );
    assert!(text.contains("— 595 more matching files"), "{text}");

    let all = GlobTool::default()
        .call(json!({"pattern": "*.txt", "max_results": 1000}), &ctx)
        .await
        .expect("glob");
    let all_text = all.content;
    assert_eq!(all_text.lines().count(), 600);
    assert_eq!(all_text.lines().next(), Some("f599.txt"));
    assert_eq!(all_text.lines().last(), Some("f0.txt"));
}
