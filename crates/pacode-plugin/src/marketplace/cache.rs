//! On-disk cache of fetched marketplace indexes.
//!
//! Ownership: the cache owns its directory and nothing else; entries expire by
//! TTL and the whole directory is bounded by a file count. A corrupt or
//! unreadable entry behaves exactly like a miss, and a fetch failure serves the
//! stale copy rather than leaving the user with nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::MarketplaceIndex;

/// Bumped whenever the stored shape changes; an older or newer file is discarded
/// rather than misparsed.
pub const CACHE_FORMAT_VERSION: u32 = 1;
/// How long a fetched index is considered fresh.
pub const DEFAULT_TTL_SECS: u64 = 6 * 60 * 60;
/// Most cached indexes kept; the oldest are dropped past this.
pub const MAX_ENTRIES: usize = 32;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    fetched_at_ms: u64,
    index: MarketplaceIndex,
}

/// An index and whether it came from a live fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedIndex {
    pub index: MarketplaceIndex,
    pub fetched_at_ms: u64,
    /// True when the copy is past its TTL and was served because the fetch failed.
    pub stale: bool,
}

pub struct IndexCache {
    dir: PathBuf,
    ttl_secs: u64,
}

impl IndexCache {
    pub fn new(dir: PathBuf, ttl_secs: u64) -> Self {
        Self { dir, ttl_secs }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    /// The cached index for `key`, if a readable one is there. `None` for a miss,
    /// a corrupt file or a version mismatch — every one of which is just a miss.
    pub fn get(&self, key: &str, now_ms: u64) -> Option<CachedIndex> {
        let path = self.path_for(key);
        let bytes = std::fs::read(&path).ok()?;
        let file: CacheFile = match serde_json::from_slice(&bytes) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("discarding unreadable marketplace cache {path:?}: {e}");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };
        if file.version != CACHE_FORMAT_VERSION {
            log::warn!(
                "discarding marketplace cache {path:?}: format {} is not {CACHE_FORMAT_VERSION}",
                file.version
            );
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let age = now_ms.saturating_sub(file.fetched_at_ms);
        Some(CachedIndex {
            index: file.index,
            fetched_at_ms: file.fetched_at_ms,
            stale: age > self.ttl_secs.saturating_mul(1000),
        })
    }

    /// Store an index. A write failure is logged and otherwise ignored: the
    /// cache is an optimisation, never a requirement.
    pub fn put(&self, key: &str, index: &MarketplaceIndex, now_ms: u64) {
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            log::warn!("cannot create the marketplace cache directory: {e}");
            return;
        }
        let file = CacheFile {
            version: CACHE_FORMAT_VERSION,
            fetched_at_ms: now_ms,
            index: index.clone(),
        };
        let path = self.path_for(key);
        match serde_json::to_vec(&file) {
            Ok(bytes) => {
                if let Err(e) = write_atomically(&path, &bytes) {
                    log::warn!("cannot write the marketplace cache {path:?}: {e}");
                }
            }
            Err(e) => log::warn!("cannot encode the marketplace cache: {e}"),
        }
        self.enforce_cap();
    }

    /// Drop the oldest entries past the cap.
    fn enforce_cap(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| {
                let modified = e.metadata().ok()?.modified().ok()?;
                Some((modified, e.path()))
            })
            .collect();
        if files.len() <= MAX_ENTRIES {
            return;
        }
        files.sort_by_key(|(t, _)| *t);
        for (_, path) in files.iter().take(files.len() - MAX_ENTRIES) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Write through a temporary file in the same directory, so an interrupted write
/// never leaves a half-file that would read back as corrupt.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
