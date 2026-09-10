//! Persistent on-disk and in-memory model catalog cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use pacode_types::ModelInfo;
use serde::{Deserialize, Serialize};

use crate::Provider;

#[cfg(test)]
#[path = "catalog_cache_tests.rs"]
mod catalog_cache_tests;

pub const CATALOG_CACHE_VERSION: u32 = 1;
pub const DEFAULT_MAX_MODELS_PER_PROVIDER: usize = 500;
pub const DEFAULT_CATALOG_TTL_SECS: u64 = 86400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogDiskCache {
    pub version: u32,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderCacheRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderCacheRecord {
    pub fetched_at: u64,
    pub models: Vec<ModelInfo>,
}

/// Load cache from disk. Returns `None` on missing, corrupt, or version-mismatched files.
pub fn load_disk_cache(path: &Path) -> Option<CatalogDiskCache> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return None;
        }
        Err(err) => {
            let path_display = path.display();
            log::warn!("failed to read model catalog cache at {path_display}: {err}");
            return None;
        }
    };

    let cache: CatalogDiskCache = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(err) => {
            let path_display = path.display();
            log::warn!("failed to parse model catalog cache at {path_display}: {err}");
            return None;
        }
    };

    if cache.version != CATALOG_CACHE_VERSION {
        let path_display = path.display();
        let got_version = cache.version;
        log::warn!(
            "model catalog cache version mismatch at {path_display} (expected {CATALOG_CACHE_VERSION}, got {got_version}); discarding"
        );
        return None;
    }

    Some(cache)
}

/// Persist cache to disk atomically via temporary file.
pub fn save_disk_cache(path: &Path, cache: &CatalogDiskCache) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        let parent_display = parent.display();
        log::warn!("failed to create catalog cache dir {parent_display}: {err}");
        return;
    }

    let json = match serde_json::to_string_pretty(cache) {
        Ok(j) => j,
        Err(err) => {
            log::warn!("failed to serialize catalog cache: {err}");
            return;
        }
    };

    let pid = std::process::id();
    let tmp_path = format!("{}.tmp.{pid}", path.display());
    if let Err(err) = std::fs::write(&tmp_path, json) {
        log::warn!("failed to write temp catalog cache to {tmp_path}: {err}");
        return;
    }

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let path_display = path.display();
        log::warn!("failed to rename {tmp_path} to {path_display}: {err}");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

pub struct CatalogCache {
    cache_path: RwLock<Option<PathBuf>>,
    ttl_secs: AtomicU64,
    max_models_per_provider: usize,
    memory: Mutex<BTreeMap<String, ProviderCacheRecord>>,
    in_flight: Mutex<BTreeMap<String, Arc<tokio::sync::Notify>>>,
    disk_lock: Mutex<()>,
}

impl CatalogCache {
    pub fn new(cache_path: Option<PathBuf>, ttl_secs: u64, max_models_per_provider: usize) -> Self {
        let memory = if let Some(ref path) = cache_path {
            load_disk_cache(path)
                .map(|c| c.providers)
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };

        Self {
            cache_path: RwLock::new(cache_path),
            ttl_secs: AtomicU64::new(ttl_secs),
            max_models_per_provider,
            memory: Mutex::new(memory),
            in_flight: Mutex::new(BTreeMap::new()),
            disk_lock: Mutex::new(()),
        }
    }

    pub fn set_cache_path(&self, path: PathBuf) {
        if let Some(disk) = load_disk_cache(&path) {
            let mut mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
            for (k, v) in disk.providers {
                mem.entry(k).or_insert(v);
            }
        }
        if let Ok(mut p) = self.cache_path.write() {
            *p = Some(path);
        }
    }

    pub fn set_ttl_secs(&self, ttl: u64) {
        self.ttl_secs.store(ttl, Ordering::SeqCst);
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs.load(Ordering::Relaxed)
    }

    pub fn max_models_per_provider(&self) -> usize {
        self.max_models_per_provider
    }

    pub fn get_cached_models(&self, provider_id: &str) -> Option<Vec<ModelInfo>> {
        let mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
        mem.get(provider_id).map(|r| r.models.clone())
    }

    pub fn seed_memory_cache(&self, provider_id: &str, fetched_at: u64, models: Vec<ModelInfo>) {
        let mut mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
        mem.insert(
            provider_id.to_string(),
            ProviderCacheRecord { fetched_at, models },
        );
    }

    fn read_provider_from_disk(&self, provider_id: &str) -> Option<ProviderCacheRecord> {
        let path = self
            .cache_path
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        let disk = load_disk_cache(&path)?;
        disk.providers.get(provider_id).cloned()
    }

    fn persist_provider_record(&self, provider_id: &str, record: ProviderCacheRecord) {
        let path = {
            let guard = self.cache_path.read().unwrap_or_else(|p| p.into_inner());
            guard.clone()
        };
        let Some(path) = path else { return };

        let _lock = self.disk_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut disk_cache = load_disk_cache(&path).unwrap_or_else(|| CatalogDiskCache {
            version: CATALOG_CACHE_VERSION,
            providers: BTreeMap::new(),
        });
        disk_cache.providers.insert(provider_id.to_string(), record);
        save_disk_cache(&path, &disk_cache);
    }

    fn trigger_background_refresh(
        self: &Arc<Self>,
        provider_id: &str,
        provider: &Arc<dyn Provider>,
    ) {
        let should_spawn = {
            let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            if in_flight.contains_key(provider_id) {
                false
            } else {
                in_flight.insert(
                    provider_id.to_string(),
                    Arc::new(tokio::sync::Notify::new()),
                );
                true
            }
        };

        if should_spawn {
            let cache = Arc::clone(self);
            let provider = Arc::clone(provider);
            let pid = provider_id.to_string();
            tokio::spawn(async move {
                let _ = cache.perform_refresh(&pid, provider.as_ref()).await;
            });
        }
    }

    async fn perform_refresh(&self, provider_id: &str, provider: &dyn Provider) -> Vec<ModelInfo> {
        struct InFlightGuard<'a> {
            in_flight: &'a Mutex<BTreeMap<String, Arc<tokio::sync::Notify>>>,
            id: &'a str,
        }
        impl Drop for InFlightGuard<'_> {
            fn drop(&mut self) {
                let notify = {
                    let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
                    in_flight.remove(self.id)
                };
                if let Some(n) = notify {
                    n.notify_waiters();
                }
            }
        }
        let _guard = InFlightGuard {
            in_flight: &self.in_flight,
            id: provider_id,
        };

        match provider.refresh_models().await {
            Ok(mut models) => {
                if models.len() > self.max_models_per_provider {
                    models.truncate(self.max_models_per_provider);
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let record = ProviderCacheRecord {
                    fetched_at: now,
                    models: models.clone(),
                };
                {
                    let mut mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
                    mem.insert(provider_id.to_string(), record.clone());
                }
                self.persist_provider_record(provider_id, record);
                models
            }
            Err(err) => {
                log::warn!("failed to refresh models for provider '{provider_id}': {err}");
                let mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
                mem.get(provider_id)
                    .map(|r| r.models.clone())
                    .unwrap_or_default()
            }
        }
    }

    /// Retrieve models for `provider_id`. Serves immediately if present in memory or
    /// on disk; triggers background refresh if stale; performs cold fetch if absent.
    pub async fn get_models_for_provider(
        self: &Arc<Self>,
        provider_id: &str,
        provider: &Arc<dyn Provider>,
    ) -> Vec<ModelInfo> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let ttl = self.ttl_secs.load(Ordering::Relaxed);

        let cached_record = {
            let mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(provider_id).cloned()
        };

        let cached_record = match cached_record {
            Some(rec) => Some(rec),
            None => {
                let disk_rec = self.read_provider_from_disk(provider_id);
                if let Some(ref rec) = disk_rec {
                    let mut mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
                    mem.insert(provider_id.to_string(), rec.clone());
                }
                disk_rec
            }
        };

        if let Some(record) = cached_record {
            let is_stale = now.saturating_sub(record.fetched_at) > ttl;
            if is_stale {
                self.trigger_background_refresh(provider_id, provider);
            }
            return record.models;
        }

        let notify = {
            let mut in_flight = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(existing) = in_flight.get(provider_id) {
                Some(Arc::clone(existing))
            } else {
                in_flight.insert(
                    provider_id.to_string(),
                    Arc::new(tokio::sync::Notify::new()),
                );
                None
            }
        };

        if let Some(notify) = notify {
            notify.notified().await;
            let mem = self.memory.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(provider_id)
                .map(|r| r.models.clone())
                .unwrap_or_default()
        } else {
            self.perform_refresh(provider_id, provider.as_ref()).await
        }
    }
}
