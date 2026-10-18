//! Content-addressed image store: `<root>/<sha256>.<ext>`.
//!
//! Image bytes are stored once on disk; messages, transcript items and the TUI refer to
//! them by hash and load lazily.
//!
//! * Owner: the daemon fills it (`put` while handling a user message) and runs
//!   [`ImageStore::evict`] once at start; the TUI and the provider transports only read.
//!   Both processes share the directory (`Paths::images_dir`).
//! * Cap: [`MAX_STORE_BYTES`] and [`MAX_STORE_FILES`]; the least recently used files
//!   (by mtime, bumped on every `put`/`load`) go first. No timer: eviction runs at
//!   daemon start only.
//! * Invalidation: files are immutable (the name is the hash of the content); a file
//!   evicted while history still references it makes `load` fail with
//!   [`StoreError::NotFound`] and callers degrade to a text note.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;

/// Total size cap of the store.
pub const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
/// File count cap of the store.
pub const MAX_STORE_FILES: usize = 2000;

const EXTENSIONS: [&str; 4] = ["png", "jpg", "webp", "gif"];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("image store io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid image hash: {0}")]
    InvalidHash(String),
    #[error("image {0} is not in the store")]
    NotFound(String),
    #[error("image store is not initialised")]
    NotInitialised,
}

/// What an eviction pass removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvictReport {
    pub removed_files: usize,
    pub removed_bytes: u64,
    pub kept_files: usize,
    pub kept_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ImageStore {
    root: PathBuf,
}

/// Lowercase hex SHA-256.
pub fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn touch(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

impl ImageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of a stored image, when present.
    pub fn path_of(&self, hash: &str) -> Option<PathBuf> {
        if !valid_hash(hash) {
            return None;
        }
        EXTENSIONS
            .iter()
            .chain(["bin"].iter())
            .map(|ext| self.root.join(format!("{hash}.{ext}")))
            .find(|p| p.is_file())
    }

    /// Store `bytes` and return their hash. Storing the same bytes twice is a no-op
    /// that only refreshes the LRU timestamp.
    pub fn put(&self, bytes: &[u8], mime: &str) -> Result<String, StoreError> {
        let hash = hash_hex(bytes);
        if let Some(existing) = self.path_of(&hash) {
            touch(&existing);
            return Ok(hash);
        }
        std::fs::create_dir_all(&self.root)?;
        let final_path = self.root.join(format!("{hash}.{}", extension_for(mime)));
        // One temp name per call: two threads storing the same bytes must not share
        // (and then rename away) each other's file.
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self
            .root
            .join(format!("{hash}.tmp-{}-{seq}", std::process::id()));
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.flush()?;
        }
        if let Err(e) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            // A concurrent writer of the same content may have won the rename.
            if !final_path.exists() {
                return Err(e.into());
            }
        }
        Ok(hash)
    }

    pub fn load(&self, hash: &str) -> Result<Vec<u8>, StoreError> {
        if !valid_hash(hash) {
            return Err(StoreError::InvalidHash(hash.chars().take(16).collect()));
        }
        let path = self
            .path_of(hash)
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))?;
        let bytes = std::fs::read(&path)?;
        touch(&path);
        Ok(bytes)
    }

    /// Remove least-recently-used files until at most `max_files` files and
    /// `max_bytes` bytes remain; stray temp files are always removed.
    pub fn evict(&self, max_bytes: u64, max_files: usize) -> Result<EvictReport, StoreError> {
        let mut report = EvictReport::default();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e.into()),
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name();
            if name.to_string_lossy().contains(".tmp-") {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((mtime, meta.len(), path));
        }
        files.sort_by_key(|f| f.0);
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let mut count = files.len();
        for (_, len, path) in files {
            if total <= max_bytes && count <= max_files {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                count -= 1;
                report.removed_files += 1;
                report.removed_bytes += len;
            }
        }
        report.kept_files = count;
        report.kept_bytes = total;
        Ok(report)
    }
}

static GLOBAL: OnceLock<ImageStore> = OnceLock::new();

/// Source of unique temp-file suffixes for `ImageStore::put`.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Set the process-wide store root (daemon and TUI call this once at start). Returns
/// `false` when a root was already set; the first one wins.
pub fn init(root: impl Into<PathBuf>) -> bool {
    GLOBAL.set(ImageStore::new(root)).is_ok()
}

pub fn global() -> Option<&'static ImageStore> {
    GLOBAL.get()
}

/// `put` on the process-wide store.
pub fn put(bytes: &[u8], mime: &str) -> Result<String, StoreError> {
    global().ok_or(StoreError::NotInitialised)?.put(bytes, mime)
}

/// `load` from the process-wide store.
pub fn load(hash: &str) -> Result<Vec<u8>, StoreError> {
    global().ok_or(StoreError::NotInitialised)?.load(hash)
}
