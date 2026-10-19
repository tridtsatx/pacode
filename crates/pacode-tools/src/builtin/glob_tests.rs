use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

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

    let out = GlobTool
        .call(json!({"pattern": "*.txt", "max_results": 5}), &ctx)
        .await
        .expect("glob");
    let text = out.content;
    let names: Vec<&str> = text.lines().collect();
    assert_eq!(
        names,
        ["f599.txt", "f598.txt", "f597.txt", "f596.txt", "f595.txt"]
    );

    let all = GlobTool
        .call(json!({"pattern": "*.txt", "max_results": 1000}), &ctx)
        .await
        .expect("glob");
    let all_text = all.content;
    assert_eq!(all_text.lines().count(), 600);
    assert_eq!(all_text.lines().next(), Some("f599.txt"));
    assert_eq!(all_text.lines().last(), Some("f0.txt"));
}
