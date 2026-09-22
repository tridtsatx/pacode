use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pacode_types::McpServerConfig;

use super::*;

fn fake_mcp_config() -> McpServerConfig {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fake_mcp.py");
    McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_string_lossy().to_string()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        enabled: true,
        lazy: true,
        timeout_secs: 10,
    }
}

#[tokio::test]
async fn test_idle_client_reaped_at_next_pool_access() {
    let mut servers = BTreeMap::new();
    servers.insert("test-srv".to_string(), fake_mcp_config());
    let pool = McpPool::new(servers, None, None);
    pool.set_idle_timeout(Duration::from_millis(50));

    // Discover tools so the schema is cached in memory
    let tools = pool.list_tools("test-srv").await.unwrap();
    assert!(!tools.is_empty());
    assert!(pool.is_running_async("test-srv").await);

    // Wait past threshold
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Next pool access should reap the expired client and serve tools from cache
    let all_tools = pool.list_all_tools().await;
    assert!(!all_tools.is_empty());
    assert!(!pool.is_running_async("test-srv").await);

    // Starting again works (returns to lazy state)
    let _client2 = pool.get_or_start_client("test-srv").await.unwrap();
    assert!(pool.is_running_async("test-srv").await);
}

#[tokio::test]
async fn test_idle_client_reaped_via_statuses() {
    let mut servers = BTreeMap::new();
    servers.insert("test-srv".to_string(), fake_mcp_config());
    let pool = McpPool::new(servers, None, None);
    pool.set_idle_timeout(Duration::from_millis(50));

    let _tools = pool.list_tools("test-srv").await.unwrap();
    assert!(pool.is_running_async("test-srv").await);

    tokio::time::sleep(Duration::from_millis(80)).await;

    // calling statuses() triggers reap
    let _statuses = pool.statuses();
    assert!(!pool.is_running_async("test-srv").await);
}

#[tokio::test]
async fn test_client_inside_threshold_not_reaped() {
    let mut servers = BTreeMap::new();
    servers.insert("test-srv".to_string(), fake_mcp_config());
    let pool = McpPool::new(servers, None, None);
    pool.set_idle_timeout(Duration::from_secs(60));

    let _client = pool.get_or_start_client("test-srv").await.unwrap();
    assert!(pool.is_running_async("test-srv").await);

    // Access pool; client is well within 60s threshold
    let _tools = pool.list_all_tools().await;
    assert!(pool.is_running_async("test-srv").await);
}

#[tokio::test]
async fn test_call_in_flight_never_reaped() {
    let mut servers = BTreeMap::new();
    servers.insert("test-srv".to_string(), fake_mcp_config());
    let pool = McpPool::new(servers, None, None);
    pool.set_idle_timeout(Duration::from_millis(30));

    // Acquire client with in-flight guard
    let (_client, guard) = pool.acquire_client("test-srv").await.unwrap();
    assert!(pool.is_running_async("test-srv").await);

    // Sleep past the 30ms idle threshold while call is in flight
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Pool access must NOT reap in-flight call
    let reaped = pool.reap_idle().await;
    assert!(reaped.is_empty());
    assert!(pool.is_running_async("test-srv").await);

    // Drop in-flight guard
    drop(guard);

    // Sleep past threshold again
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Now it should be reaped on next pool access
    let reaped = pool.reap_idle().await;
    assert_eq!(reaped, vec!["test-srv".to_string()]);
    assert!(!pool.is_running_async("test-srv").await);
}

/// The `*_cached` listers answer from the disk schema cache without ever
/// spawning a server: every config names a command that cannot start, so any
/// live call would fail loudly — the cached reads still succeed. A lazy
/// server's cache hit also marks the status `Ready` (mirroring the lazy
/// branch of `list_all_tools`); a non-lazy one does not.
#[test]
fn test_cached_listers_serve_disk_schema_without_spawning() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let cache_dir = temp_dir.path().to_path_buf();

    let bogus = McpServerConfig {
        command: "bogus_command_that_does_not_exist_xyz_12345".to_string(),
        args: vec![],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        enabled: true,
        lazy: false,
        timeout_secs: 5,
    };

    let tools = vec![McpToolInfo {
        name: "echo".to_string(),
        description: "echoes".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }];
    let resources = vec![McpResource {
        uri: "fake://r1".to_string(),
        name: "r1".to_string(),
        description: None,
        mime_type: None,
    }];
    let prompts = vec![McpPrompt {
        name: "p1".to_string(),
        description: None,
        arguments: vec![],
    }];

    let mut servers = BTreeMap::new();
    // Non-lazy, cached: serves the schema, does not mark Ready.
    crate::cache::save_disk_cache(&cache_dir, "eager", &bogus, &tools, &resources, &prompts);
    servers.insert("eager".to_string(), bogus.clone());
    // Lazy, cached: serves the schema and marks Ready with the counts.
    let lazy_cfg = McpServerConfig {
        lazy: true,
        ..bogus.clone()
    };
    crate::cache::save_disk_cache(&cache_dir, "lazy", &lazy_cfg, &tools, &resources, &prompts);
    servers.insert("lazy".to_string(), lazy_cfg);
    // Disabled, cached: skipped entirely — the fingerprint covers `enabled`.
    let off_cfg = McpServerConfig {
        enabled: false,
        ..bogus.clone()
    };
    crate::cache::save_disk_cache(&cache_dir, "off", &off_cfg, &tools, &resources, &prompts);
    servers.insert("off".to_string(), off_cfg);
    // Enabled, uncached: contributes nothing.
    servers.insert("uncached".to_string(), bogus.clone());

    let pool = McpPool::new(servers, Some(cache_dir), None);

    let all_tools = pool.list_all_tools_cached();
    assert_eq!(all_tools.len(), 2);
    assert_eq!(all_tools[0].0, "eager");
    assert_eq!(all_tools[1].0, "lazy");
    assert_eq!(pool.list_all_resources_cached().len(), 2);
    assert_eq!(pool.list_all_prompts_cached().len(), 2);

    // Nothing spawned: the bogus command would have failed any live call.
    for name in ["eager", "lazy", "off", "uncached"] {
        assert!(!pool.is_running(name), "{name} must not be running");
    }

    let statuses = pool.statuses();
    let ready: Vec<_> = statuses
        .iter()
        .filter(|(_, s)| matches!(s, ServerStatus::Ready { .. }))
        .collect();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].0, "lazy");
    assert_eq!(
        ready[0].1,
        ServerStatus::Ready {
            tools: 1,
            resources: 1,
            prompts: 1,
        }
    );
}

#[tokio::test]
async fn test_zero_threshold_disables_reaping() {
    let mut servers = BTreeMap::new();
    servers.insert("test-srv".to_string(), fake_mcp_config());
    let pool = McpPool::new(servers, None, None);
    pool.set_idle_timeout_secs(0);

    let _client = pool.get_or_start_client("test-srv").await.unwrap();
    assert!(pool.is_running_async("test-srv").await);

    // Wait and access pool
    tokio::time::sleep(Duration::from_millis(60)).await;
    let _tools = pool.list_all_tools().await;

    // Threshold 0 disables reaping
    assert!(pool.is_running_async("test-srv").await);
    let reaped = pool.reap_idle().await;
    assert!(reaped.is_empty());
    assert!(pool.is_running_async("test-srv").await);
}
