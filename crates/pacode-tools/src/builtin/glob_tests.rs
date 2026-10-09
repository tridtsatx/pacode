use std::sync::Arc;

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

/// The walk no longer carries mtimes; glob stats its matches itself and still
/// lists them newest first (ties in path order).
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
