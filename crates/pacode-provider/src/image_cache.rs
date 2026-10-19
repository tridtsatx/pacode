//! Process-wide LRU of base64 payloads of stored images (finding P1-12).
//!
//! Every request re-sends the images in history; without this cache each step re-reads
//! and re-encodes them. Keys are content hashes of the image store, so entries are
//! immutable and never need invalidation.
//!
//! * Owner: this module (a single `static`), shared by all providers in the process.
//! * Cap: [`CAP_BYTES`] of base64 text in total; least recently used entries are evicted
//!   first, and an image larger than the whole cap is never cached.
//! * Invalidation: none (hash = content); the cache only shrinks by eviction.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::ProviderError;

#[cfg(test)]
#[path = "image_cache_tests.rs"]
mod image_cache_tests;

/// Total base64 bytes kept (32 MB).
pub(crate) const CAP_BYTES: usize = 32 * 1024 * 1024;

static GLOBAL: LazyLock<Base64Cache> = LazyLock::new(|| Base64Cache::new(CAP_BYTES));

/// Base64 payload of the stored image `hash`, from the cache or loaded from disk.
pub(crate) fn payload(hash: &str) -> Result<Arc<str>, ProviderError> {
    GLOBAL.get_or_load(hash, || {
        pacode_image::store::load(hash)
            .map_err(|e| ProviderError::Config(format!("image {hash} unavailable: {e}")))
    })
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, (Arc<str>, u64)>,
    bytes: usize,
    tick: u64,
}

pub(crate) struct Base64Cache {
    cap: usize,
    inner: Mutex<Inner>,
}

impl Base64Cache {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Cached payload for `hash`; on a miss `load` produces the raw bytes (outside the lock).
    pub(crate) fn get_or_load(
        &self,
        hash: &str,
        load: impl FnOnce() -> Result<Vec<u8>, ProviderError>,
    ) -> Result<Arc<str>, ProviderError> {
        if let Some(hit) = self.lookup(hash) {
            return Ok(hit);
        }
        let encoded: Arc<str> = Arc::from(STANDARD.encode(load()?));
        self.insert(hash, &encoded);
        Ok(encoded)
    }

    fn lookup(&self, hash: &str) -> Option<Arc<str>> {
        let mut inner = self.inner.lock().ok()?;
        inner.tick += 1;
        let tick = inner.tick;
        let entry = inner.entries.get_mut(hash)?;
        entry.1 = tick;
        Some(Arc::clone(&entry.0))
    }

    fn insert(&self, hash: &str, value: &Arc<str>) {
        if value.len() > self.cap {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.tick += 1;
        let tick = inner.tick;
        if let Some((old, _)) = inner
            .entries
            .insert(hash.to_string(), (Arc::clone(value), tick))
        {
            inner.bytes -= old.len();
        }
        inner.bytes += value.len();
        while inner.bytes > self.cap {
            let oldest = inner
                .entries
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone());
            let Some(key) = oldest else { break };
            if let Some((evicted, _)) = inner.entries.remove(&key) {
                inner.bytes -= evicted.len();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn total_bytes(&self) -> usize {
        self.inner.lock().map(|i| i.bytes).unwrap_or(0)
    }
}
