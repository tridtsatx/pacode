use std::sync::Arc;
use std::time::Duration;

use pacode_types::ModelInfo;

use super::*;
use crate::mock::MockProvider;

fn make_model_info(provider: &str, model: &str) -> ModelInfo {
    ModelInfo {
        route: pacode_types::ModelRoute::new(provider, model),
        display_name: format!("Model {model}"),
        context_window: Some(32000),
        supports_reasoning: false,
        pricing: None,
    }
}

#[tokio::test]
async fn test_fresh_cache_is_served_without_http_call() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    let cache = Arc::new(CatalogCache::new(
        Some(cache_file),
        86400,
        DEFAULT_MAX_MODELS_PER_PROVIDER,
    ));

    let mock = Arc::new(MockProvider::new("mock-prov"));
    mock.set_models(vec![make_model_info("mock-prov", "live-model")]);

    // Seed the cache as fresh (fetched_at = now)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    cache.seed_memory_cache(
        "mock-prov",
        now,
        vec![make_model_info("mock-prov", "cached-model")],
    );

    let prov: Arc<dyn Provider> = mock.clone();
    let models = cache.get_models_for_provider("mock-prov", &prov).await;

    // Must return cached model, no live call to mock provider
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].route.model, "cached-model");
    assert_eq!(mock.list_models_count(), 0);
}

#[tokio::test]
async fn test_stale_cache_served_immediately_and_single_refresh_with_concurrent_callers() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    // TTL = 10 seconds
    let cache = Arc::new(CatalogCache::new(
        Some(cache_file),
        10,
        DEFAULT_MAX_MODELS_PER_PROVIDER,
    ));

    let mock = Arc::new(MockProvider::new("mock-prov"));
    mock.set_models(vec![make_model_info("mock-prov", "freshly-fetched-model")]);
    // Introduce a short delay in the provider so concurrent calls overlap
    mock.set_list_models_delay(Duration::from_millis(50));

    // Seed cache with a timestamp 100 seconds ago (stale)
    let old_timestamp = 1000;
    cache.seed_memory_cache(
        "mock-prov",
        old_timestamp,
        vec![make_model_info("mock-prov", "stale-model")],
    );

    // Call get_models_for_provider concurrently from 5 tasks
    let prov: Arc<dyn Provider> = mock.clone();
    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&cache);
        let p = Arc::clone(&prov);
        handles.push(tokio::spawn(async move {
            c.get_models_for_provider("mock-prov", &p).await
        }));
    }

    // Every caller must immediately receive the stale models
    for handle in handles {
        let models = handle.await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].route.model, "stale-model");
    }

    // Give the background refresh task time to finish
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Exactly one refresh should have been launched across all concurrent callers
    assert_eq!(mock.list_models_count(), 1);

    // Subsequent call sees the fresh model from the completed refresh
    let refreshed_models = cache.get_models_for_provider("mock-prov", &prov).await;
    assert_eq!(refreshed_models.len(), 1);
    assert_eq!(refreshed_models[0].route.model, "freshly-fetched-model");
}

#[tokio::test]
async fn test_corrupt_cache_file_is_treated_as_cold() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    // Write corrupted JSON into the cache file
    std::fs::write(&cache_file, "{ not-valid-json: true [}").unwrap();

    let cache = Arc::new(CatalogCache::new(
        Some(cache_file.clone()),
        86400,
        DEFAULT_MAX_MODELS_PER_PROVIDER,
    ));

    let mock = Arc::new(MockProvider::new("mock-prov"));
    mock.set_models(vec![make_model_info("mock-prov", "live-from-cold")]);

    let prov: Arc<dyn Provider> = mock.clone();
    let models = cache.get_models_for_provider("mock-prov", &prov).await;

    // Treated as cold: fetched live models
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].route.model, "live-from-cold");
    assert_eq!(mock.list_models_count(), 1);

    // Disk cache should now be validly overwritten
    let reloaded = load_disk_cache(&cache_file);
    assert!(reloaded.is_some());
    let records = reloaded.unwrap().providers;
    assert_eq!(records["mock-prov"].models[0].route.model, "live-from-cold");
}

#[tokio::test]
async fn test_version_mismatch_is_discarded() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    // Write a cache file with an incompatible format version
    let mismatched = CatalogDiskCache {
        version: CATALOG_CACHE_VERSION + 1,
        providers: BTreeMap::new(),
    };
    let json = serde_json::to_string(&mismatched).unwrap();
    std::fs::write(&cache_file, json).unwrap();

    let cache = Arc::new(CatalogCache::new(
        Some(cache_file.clone()),
        86400,
        DEFAULT_MAX_MODELS_PER_PROVIDER,
    ));

    let mock = Arc::new(MockProvider::new("mock-prov"));
    mock.set_models(vec![make_model_info("mock-prov", "live-after-mismatch")]);

    let prov: Arc<dyn Provider> = mock.clone();
    let models = cache.get_models_for_provider("mock-prov", &prov).await;

    // Discarded and treated as cold: fetched live models
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].route.model, "live-after-mismatch");
    assert_eq!(mock.list_models_count(), 1);
}

#[tokio::test]
async fn test_per_provider_cap_is_enforced() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    // Cap at 5 models for easy deterministic assertion
    let cap = 5;
    let cache = Arc::new(CatalogCache::new(Some(cache_file.clone()), 86400, cap));

    // Provide 10 models
    let mut ten_models = Vec::new();
    for i in 0..10 {
        ten_models.push(make_model_info("mock-prov", &format!("model-{i:02}")));
    }

    let mock = Arc::new(MockProvider::new("mock-prov"));
    mock.set_models(ten_models);

    let prov: Arc<dyn Provider> = mock.clone();
    let models = cache.get_models_for_provider("mock-prov", &prov).await;

    // Capped to first 5 models deterministically
    assert_eq!(models.len(), 5);
    for (i, m) in models.iter().enumerate() {
        assert_eq!(m.route.model, format!("model-{i:02}"));
    }

    // Disk cache also capped to 5
    let reloaded = load_disk_cache(&cache_file).unwrap();
    let disk_models = &reloaded.providers["mock-prov"].models;
    assert_eq!(disk_models.len(), 5);
    for (i, m) in disk_models.iter().enumerate() {
        assert_eq!(m.route.model, format!("model-{i:02}"));
    }
}

#[tokio::test]
async fn test_failed_refresh_leaves_previous_cache_in_place() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache_file = temp_dir.path().join("catalog.json");

    // Short TTL (1s) to force refresh
    let cache = Arc::new(CatalogCache::new(
        Some(cache_file),
        1,
        DEFAULT_MAX_MODELS_PER_PROVIDER,
    ));

    let mock = Arc::new(MockProvider::new("mock-prov"));
    // Seed stale cache
    cache.seed_memory_cache(
        "mock-prov",
        100, // old timestamp
        vec![make_model_info("mock-prov", "previous-good-model")],
    );

    // Mock provider fails on refresh
    mock.set_list_models_error(Some("network unreachable".to_string()));

    let prov: Arc<dyn Provider> = mock.clone();
    let models = cache.get_models_for_provider("mock-prov", &prov).await;

    // Served stale model immediately
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].route.model, "previous-good-model");

    // Wait for background refresh to attempt and fail
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(mock.list_models_count(), 1);

    // The previous cached model is still in place!
    let subsequent = cache.get_models_for_provider("mock-prov", &prov).await;
    assert_eq!(subsequent.len(), 1);
    assert_eq!(subsequent[0].route.model, "previous-good-model");
}
