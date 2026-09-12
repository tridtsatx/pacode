//! `WalkCache` tests: caching, TTL, caps, path invalidation; plus the
//! tool-level check that a `write` invalidates a cached `glob` scan.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{WalkCache, WalkOptions};
use crate::Tool;
use crate::builtin::glob::GlobTool;
use crate::builtin::write::WriteTool;
use crate::test_support::{StubToolHost, stub_ctx};

const NO_HIDDEN: WalkOptions = WalkOptions {
    include_hidden: false,
    max_depth: None,
};

fn opts(include_hidden: bool) -> WalkOptions {
    WalkOptions {
        include_hidden,
        max_depth: None,
    }
}

#[test]
fn second_collect_is_served_from_cache() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/b.rs"), "fn b() {}\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 16, 1024);
    let cancel = CancellationToken::new();

    let first = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    let second = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    assert!(
        Arc::ptr_eq(&first, &second),
        "second collect must reuse the cached scan"
    );
    assert_eq!(cache.len(), 1);
    let rels: Vec<PathBuf> = first.iter().map(|e| e.rel.clone()).collect();
    assert!(rels.contains(&PathBuf::from("a.rs")));
    assert!(rels.contains(&PathBuf::from("sub/b.rs")));
    assert!(rels.contains(&PathBuf::from("sub")));
}

#[test]
fn invalidate_path_drops_covering_scan() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 16, 1024);
    let cancel = CancellationToken::new();

    let first = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    cache.invalidate_path(&tmp.path().join("a.rs"));
    assert_eq!(cache.len(), 0);
    let second = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "post-invalidate scan must re-walk"
    );
}

#[test]
fn invalidate_outside_root_keeps_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 16, 1024);
    let cancel = CancellationToken::new();

    let first = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    cache.invalidate_path(&other.path().join("unrelated.rs"));
    let second = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn expired_entries_are_rescanned() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::ZERO, 16, 1024);
    let cancel = CancellationToken::new();

    let first = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    let second = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "zero TTL never hits the cache"
    );
}

#[test]
fn options_segregate_scans() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("vis.rs"), "x\n").unwrap();
    std::fs::write(tmp.path().join(".hidden.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 16, 1024);
    let cancel = CancellationToken::new();

    let hidden = cache.collect(tmp.path(), opts(true), &cancel).unwrap();
    let visible = cache.collect(tmp.path(), opts(false), &cancel).unwrap();
    assert_eq!(cache.len(), 2);
    assert!(
        hidden
            .iter()
            .any(|e| e.rel.as_path() == std::path::Path::new(".hidden.rs"))
    );
    assert!(
        !visible
            .iter()
            .any(|e| e.rel.as_path() == std::path::Path::new(".hidden.rs"))
    );
}

#[test]
fn oversized_scan_is_served_but_not_cached() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..4 {
        std::fs::write(tmp.path().join(format!("f{i}.rs")), "x\n").unwrap();
    }

    let cache = WalkCache::new(Duration::from_secs(60), 16, 2);
    let cancel = CancellationToken::new();
    let entries = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(cache.len(), 0, "oversized scans are never stored");
}

#[test]
fn oldest_root_is_evicted_past_the_cap() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("a.rs"), "x\n").unwrap();
    std::fs::write(b.path().join("b.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 1, 1024);
    let cancel = CancellationToken::new();
    cache.collect(a.path(), NO_HIDDEN, &cancel).unwrap();
    cache.collect(b.path(), NO_HIDDEN, &cancel).unwrap();
    assert_eq!(cache.len(), 1);
    // The older scan was evicted; `a` re-walks.
    cache.collect(a.path(), NO_HIDDEN, &cancel).unwrap();
    assert_eq!(cache.len(), 1);
}

#[test]
fn equivalent_roots_share_one_scan() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/f.rs"), "x\n").unwrap();

    let cache = WalkCache::new(Duration::from_secs(60), 16, 1024);
    let cancel = CancellationToken::new();
    let direct = cache.collect(tmp.path(), NO_HIDDEN, &cancel).unwrap();
    let via_dotdot = cache
        .collect(&tmp.path().join("sub/.."), NO_HIDDEN, &cancel)
        .unwrap();
    assert!(
        Arc::ptr_eq(&direct, &via_dotdot),
        "canonical root must unify `sub/..` with the root itself"
    );
}

#[tokio::test]
async fn write_invalidates_cached_glob_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubToolHost::default());
    let mut ctx = stub_ctx(host);
    ctx.cwd = tmp.path().to_path_buf();

    let before = GlobTool
        .call(json!({"pattern": "*.txt"}), &ctx)
        .await
        .unwrap();
    assert!(!before.content.contains("new.txt"));

    WriteTool
        .call(json!({"path": "new.txt", "content": "hi\n"}), &ctx)
        .await
        .unwrap();

    let after = GlobTool
        .call(json!({"pattern": "*.txt"}), &ctx)
        .await
        .unwrap();
    assert!(
        after.content.contains("new.txt"),
        "glob must see the file written after its scan was cached:\n{}",
        after.content
    );
}
