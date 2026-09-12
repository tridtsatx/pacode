//! Raw cache-file storage (§8.2): one JSON file per list under the XDG cache
//! dir (`~/.cache/pacode/<key>.v<schema>.json`).
//!
//! Rules this module owns:
//! - a missing, oversized or corrupt file reads as `None` — the cache can
//!   never break startup;
//! - writes are atomic (tmp file + rename) and refuse payloads that would
//!   exceed [`MAX_FILE_BYTES`];
//! - [`DiskCache::enforce_caps`] evicts oldest-by-mtime cache files until the
//!   directory is under [`MAX_TOTAL_BYTES`]. Only `*.v<digits>.json` files are
//!   ever touched — `catalog.json`, `mcp/` and anything else in the directory
//!   is not ours.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// One cache file never exceeds this (§8.2).
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// All cache files together never exceed this (§8.2).
pub const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// The wire shape inside every `<key>.v<N>.json`: when it was written plus
/// the payload. The schema version lives in the file name, not here.
#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    written_at_ms: u64,
    data: T,
}

/// A successfully read cache file.
#[derive(Clone, Debug)]
pub struct Entry<T> {
    pub written_at_ms: u64,
    pub data: T,
}

/// What a cap sweep did — returned for logging.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapsReport {
    /// Files removed (oversized or evicted oldest-first).
    pub evicted: usize,
    /// Bytes freed by the removals.
    pub freed_bytes: u64,
    /// Total size of cache files remaining.
    pub total_bytes: u64,
}

/// Handle on the cache directory. Cheap to clone (a `PathBuf`).
///
/// `Default` is an empty path — every read misses and every write fails
/// benignly (logged, swallowed). `run` replaces it with the real directory
/// before the first frame.
#[derive(Clone, Debug, Default)]
pub struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<dir>/<key>.v<schema>.json`.
    fn path_for(&self, key: &str, schema: u32) -> PathBuf {
        self.dir.join(format!("{key}.v{schema}.json"))
    }

    /// Read and parse one entry. Every failure mode — missing file, oversized
    /// file, unreadable file, bad JSON, wrong envelope — is a cache miss, never
    /// an error the caller has to handle (§8.2: a corrupt cache must not keep
    /// the client from starting).
    pub fn read<T: DeserializeOwned>(&self, key: &str, schema: u32) -> Option<Entry<T>> {
        let path = self.path_for(key, schema);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return None,
        };
        if meta.len() > MAX_FILE_BYTES {
            // Larger than anything we would write — treat as corrupt and drop
            // it so the sweep does not keep carrying it.
            let _ = std::fs::remove_file(&path);
            log::warn!("cache: removed oversized file {}", path.display());
            return None;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                log::debug!("cache: unreadable {}: {e}", path.display());
                return None;
            }
        };
        match serde_json::from_slice::<Envelope<T>>(&bytes) {
            Ok(env) => Some(Entry {
                written_at_ms: env.written_at_ms,
                data: env.data,
            }),
            Err(e) => {
                log::debug!("cache: corrupt {}: {e}", path.display());
                None
            }
        }
    }

    /// Serialize and store one entry. Best-effort: every failure is logged and
    /// swallowed — a cache write must never fail a reply path. Payloads that
    /// would exceed [`MAX_FILE_BYTES`] are refused instead of written.
    pub fn write<T: Serialize>(&self, key: &str, schema: u32, data: &T) {
        let env = Envelope {
            written_at_ms: pacode_types::time::now_ms(),
            data,
        };
        let body = match serde_json::to_vec(&env) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("cache: failed to serialize {key}: {e}");
                return;
            }
        };
        if body.len() as u64 > MAX_FILE_BYTES {
            log::warn!(
                "cache: {key} is {} bytes, over the {}-byte cap — not written",
                body.len(),
                MAX_FILE_BYTES
            );
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            log::warn!("cache: cannot create {}: {e}", self.dir.display());
            return;
        }
        // Unique-ish tmp name: two clients may write the same key at once; the
        // rename makes the last one win atomically.
        let tmp = self
            .dir
            .join(format!("{key}.v{schema}.{}.tmp", std::process::id()));
        let target = self.path_for(key, schema);
        let result = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &target));
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            log::warn!("cache: failed to write {}: {e}", target.display());
        }
    }

    /// Drop one entry. Missing files are not an error.
    pub fn invalidate(&self, key: &str, schema: u32) {
        let path = self.path_for(key, schema);
        match std::fs::remove_file(&path) {
            Ok(()) => log::debug!("cache: invalidated {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::debug!("cache: cannot remove {}: {e}", path.display()),
        }
    }

    /// Sweep the directory until `*.v<N>.json` files fit the aggregate cap.
    /// Oversized singles are removed outright; then oldest-by-mtime files go
    /// until the total fits (§8.2). Runs once at client startup.
    pub fn enforce_caps(&self) -> CapsReport {
        let mut report = CapsReport::default();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(_) => return report, // no cache dir — nothing to sweep
        };
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_cache_file(name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((entry.path(), meta.len(), mtime));
        }

        report.total_bytes = files.iter().map(|(_, size, _)| *size).sum();

        // Files bigger than the per-file cap are not anything we wrote — out.
        files.retain(|(path, size, _)| {
            if *size > MAX_FILE_BYTES {
                if std::fs::remove_file(path).is_ok() {
                    report.evicted += 1;
                    report.freed_bytes += *size;
                    report.total_bytes -= *size;
                }
                false
            } else {
                true
            }
        });

        // Oldest mtime first — the stalest copy dies first (§8.2).
        files.sort_by_key(|(_, _, mtime)| *mtime);
        for (path, size, _) in files {
            if report.total_bytes <= MAX_TOTAL_BYTES {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                log::debug!("cache: evicted {}", path.display());
                report.evicted += 1;
                report.freed_bytes += size;
                report.total_bytes -= size;
            }
        }
        report
    }
}

/// Whether `name` is one of ours: `<key>.v<digits>.json`.
fn is_cache_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    let Some((key, ver)) = stem.rsplit_once(".v") else {
        return false;
    };
    !key.is_empty() && !ver.is_empty() && ver.chars().all(|c| c.is_ascii_digit())
}
