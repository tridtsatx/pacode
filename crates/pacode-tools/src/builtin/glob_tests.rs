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
