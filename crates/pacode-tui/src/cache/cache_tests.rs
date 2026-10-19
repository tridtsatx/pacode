use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pacode_types::SessionId;
use pacode_types::vcs::{CommitRow, GitStatus};

use super::disk::{MAX_FILE_BYTES, MAX_TOTAL_BYTES};
use super::*;

fn tmp_cache() -> (tempfile::TempDir, DiskCache) {
    let dir = tempfile::tempdir().unwrap();
    let cache = DiskCache::new(dir.path().join("cache"));
    (dir, cache)
}

fn session(id: u64) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(format!("pacode-test-{id:04x}")),
        name: None,
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: id,
        model: pacode_types::ModelRoute::new("p", "m"),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

fn model(name: &str) -> ModelInfo {
    ModelInfo {
        route: pacode_types::ModelRoute::new("p", name),
        display_name: name.to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
        ..Default::default()
    }
}

#[test]
fn missing_file_reads_as_none() {
    let (_tmp, cache) = tmp_cache();
    assert!(cache.read::<Vec<String>>("models", 1).is_none());
    // A missing directory is a miss too.
    let (_tmp, cache) = tmp_cache();
    let gone = DiskCache::new(cache.dir().join("nope"));
    assert!(gone.read::<Vec<String>>("models", 1).is_none());
}

#[test]
fn corrupt_file_reads_as_none_and_never_panics() {
    let (_tmp, cache) = tmp_cache();
    std::fs::create_dir_all(cache.dir()).unwrap();
    std::fs::write(cache.dir().join("models.v1.json"), b"not json {{{").unwrap();
    std::fs::write(
        cache.dir().join("sessions.v1.json"),
        b"{\"written_at_ms\":1,\"data\":\"wrong shape\"}",
    )
    .unwrap();
    assert!(cache.read::<Vec<ModelInfo>>("models", 1).is_none());
    assert!(cache.read::<Vec<SessionMeta>>("sessions", 1).is_none());
}

#[test]
fn roundtrip_write_read() {
    let (_tmp, cache) = tmp_cache();
    let sessions = vec![session(1), session(2)];
    cache.write("sessions", 1, &sessions);
    let e = cache.read::<Vec<SessionMeta>>("sessions", 1).unwrap();
    assert_eq!(e.data.len(), 2);
    assert!(e.written_at_ms > 0);
    // A different schema version is a different file — a miss.
    assert!(cache.read::<Vec<SessionMeta>>("sessions", 2).is_none());
}

#[test]
fn oversized_payload_is_refused() {
    let (_tmp, cache) = tmp_cache();
    // ~2 MiB of JSON — over the 1 MiB file cap.
    let big: Vec<String> = vec!["x".repeat(64); 40_000];
    cache.write("models", 1, &big);
    assert!(cache.read::<Vec<String>>("models", 1).is_none());
    assert!(!cache.dir().join("models.v1.json").exists());
}

#[test]
fn oversized_existing_file_is_removed_on_read() {
    let (_tmp, cache) = tmp_cache();
    std::fs::create_dir_all(cache.dir()).unwrap();
    let path = cache.dir().join("models.v1.json");
    std::fs::write(&path, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
    assert!(cache.read::<Vec<String>>("models", 1).is_none());
    assert!(!path.exists(), "oversized file should be removed");
}

#[test]
fn caps_evict_oldest_by_mtime() {
    let (_tmp, cache) = tmp_cache();
    std::fs::create_dir_all(cache.dir()).unwrap();
    // Nine ~1 MiB files ≈ 8.9 MiB — over the 8 MiB cap; evicting the oldest
    // brings the total back under it.
    let body = vec![b'x'; (MAX_FILE_BYTES - 64 * 1024) as usize];
    assert!(
        body.len() as u64 * 9 > MAX_TOTAL_BYTES,
        "test needs >8MiB of cache files"
    );
    for (name, secs_ago) in [
        ("a.v1.json", 900u64),
        ("b.v1.json", 800),
        ("c.v1.json", 700),
        ("d.v1.json", 600),
        ("e.v1.json", 500),
        ("f.v1.json", 400),
        ("g.v1.json", 300),
        ("h.v1.json", 200),
        ("i.v1.json", 100),
    ] {
        let p = cache.dir().join(name);
        std::fs::write(&p, &body).unwrap();
        let t = SystemTime::now() - Duration::from_secs(secs_ago);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    let report = cache.enforce_caps();
    assert_eq!(report.evicted, 1);
    assert!(!cache.dir().join("a.v1.json").exists());
    assert!(cache.dir().join("i.v1.json").exists());
    assert!(report.total_bytes <= MAX_TOTAL_BYTES);
}

#[test]
fn caps_ignore_foreign_files() {
    let (_tmp, cache) = tmp_cache();
    std::fs::create_dir_all(cache.dir()).unwrap();
    std::fs::write(cache.dir().join("catalog.json"), vec![b'x'; 1024]).unwrap();
    std::fs::write(cache.dir().join("notes.txt"), b"hi").unwrap();
    let report = cache.enforce_caps();
    assert_eq!(report.evicted, 0);
    assert!(cache.dir().join("catalog.json").exists());
    assert!(cache.dir().join("notes.txt").exists());
}

#[test]
fn stale_rules_per_kind() {
    let now = 1_000_000_000u64;
    let session_start = now - 60_000;
    let ttl = 86_400u64; // 24 h
    // Models: stale only past the catalog TTL.
    assert!(!is_stale(
        ListKind::Models,
        now - 3_600_000,
        now,
        session_start,
        ttl
    ));
    assert!(is_stale(
        ListKind::Models,
        now - 90_000_000,
        now,
        session_start,
        ttl
    ));
    // Marketplace: stale past 1 h.
    assert!(!is_stale(
        ListKind::Marketplace,
        now - 1_800_000,
        now,
        session_start,
        ttl
    ));
    assert!(is_stale(
        ListKind::Marketplace,
        now - 7_200_000,
        now,
        session_start,
        ttl
    ));
    // Session-scoped kinds: anything written before this run is stale.
    for kind in [
        ListKind::Plugins,
        ListKind::McpServers,
        ListKind::Sessions,
        ListKind::Commands,
        ListKind::GitLog,
    ] {
        assert!(
            !is_stale(kind, session_start + 1, now, session_start, ttl),
            "{kind:?} written this session must be fresh"
        );
        assert!(
            is_stale(kind, session_start - 1, now, session_start, ttl),
            "{kind:?} written before this session must be stale"
        );
    }
}

#[test]
fn badge_rules() {
    let mut lists = Lists::default();
    assert_eq!(lists.badge(ListKind::Models), None);
    lists.mark_sent(ListKind::Models);
    assert_eq!(lists.badge(ListKind::Models), Some(Badge::Loading));
    lists.apply_cached(ListKind::Models, true);
    assert_eq!(lists.badge(ListKind::Models), Some(Badge::OfflineCopy));
    lists.on_reply(ListKind::Models, true);
    assert_eq!(lists.badge(ListKind::Models), None);
    // A failed refresh keeps the stale badge.
    lists.apply_cached(ListKind::Sessions, true);
    lists.mark_sent(ListKind::Sessions);
    assert_eq!(lists.badge(ListKind::Sessions), Some(Badge::OfflineCopy));
    lists.on_reply(ListKind::Sessions, false);
    assert_eq!(lists.badge(ListKind::Sessions), Some(Badge::OfflineCopy));
}

#[test]
fn startup_load_filters_mismatched_contexts() {
    let (_tmp, cache) = tmp_cache();
    let cwd = PathBuf::from("/proj/a");
    cache.write(
        "mcp",
        1,
        &McpPayload {
            config_sig: "sig-a".to_string(),
            servers: Vec::new(),
        },
    );
    cache.write(
        "marketplace",
        1,
        &MarketplacePayload {
            source: "owner/repo".to_string(),
            plugins: Vec::new(),
        },
    );
    cache.write(
        &ListKind::GitLog.stem(&cwd),
        1,
        &GitPayload {
            status: GitStatus::Ready,
            commits: vec![CommitRow {
                hash: "abc".to_string(),
                ..CommitRow::default()
            }],
            has_more: false,
        },
    );

    // Matching context → all hit.
    let ctx = LoadCtx {
        cwd: cwd.clone(),
        mcp_sig: "sig-a".to_string(),
        marketplace_source: "owner/repo".to_string(),
    };
    let load = startup_load(&cache, &ctx);
    assert!(load.snapshot.mcp_servers.is_some());
    assert!(load.snapshot.marketplace.is_some());
    assert_eq!(load.snapshot.git.unwrap().data.commits.len(), 1);

    // Re-written after the matching read consumed the files (mismatched reads
    // invalidate, so re-seed first).
    cache.write(
        "mcp",
        1,
        &McpPayload {
            config_sig: "sig-a".to_string(),
            servers: Vec::new(),
        },
    );
    cache.write(
        "marketplace",
        1,
        &MarketplacePayload {
            source: "owner/repo".to_string(),
            plugins: Vec::new(),
        },
    );

    // Different sig/source/cwd → all miss, and the mismatched files are gone.
    let other = PathBuf::from("/proj/b");
    let ctx = LoadCtx {
        cwd: other.clone(),
        mcp_sig: "sig-b".to_string(),
        marketplace_source: "other/repo".to_string(),
    };
    let load = startup_load(&cache, &ctx);
    assert!(load.snapshot.mcp_servers.is_none());
    assert!(load.snapshot.marketplace.is_none());
    assert!(load.snapshot.git.is_none());
    assert!(!cache.dir().join("mcp.v1.json").exists());
    assert!(!cache.dir().join("marketplace.v1.json").exists());
    // The old project's git file is untouched — it is a different key, not an
    // eviction (per-project keys are the invalidation mechanism).
    assert!(
        cache
            .dir()
            .join(format!("{}.v1.json", ListKind::GitLog.stem(&cwd)))
            .exists()
    );
}

#[test]
fn apply_snapshot_fills_state_and_marks_stale() {
    let mut state = AppState::new(
        pacode_types::Config::default(),
        "0.0.0-test".to_string(),
        80,
        24,
    );
    let session_start = pacode_types::time::now_ms();
    let snap = CacheSnapshot {
        models: Some(Entry {
            written_at_ms: session_start - 1000, // fresh: within 24 h TTL
            data: vec![model("m")],
        }),
        sessions: Some(Entry {
            written_at_ms: session_start - 1000, // before this run → stale
            data: vec![session(7)],
        }),
        ..CacheSnapshot::default()
    };
    apply_snapshot(&mut state, snap, session_start);
    assert_eq!(state.models.len(), 1);
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(
        state.lists.models.freshness,
        Freshness::Cached { stale: false }
    );
    assert_eq!(
        state.lists.sessions.freshness,
        Freshness::Cached { stale: true }
    );
    assert_eq!(
        state.lists.badge(ListKind::Sessions),
        Some(Badge::OfflineCopy)
    );
    assert_eq!(state.lists.badge(ListKind::Plugins), None);
}
