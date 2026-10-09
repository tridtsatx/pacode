//! Shared filesystem traversal cache for `glob`, `grep` and `ls`.
//!
//! All three tools walk the workspace with `ignore::WalkBuilder` under the same
//! rules (gitignore honoured, hidden files skipped unless asked). Repeated
//! explorations — several `glob`/`grep`/`ls` calls on the same tree, or `batch`
//! ops — re-scan the same directories, so the scan itself is cached.
//!
//! Economy rule: the key is the canonical root plus the only options that
//! differ between tools; capacity is `MAX_ROOTS` scans of at most
//! `MAX_SCAN_ENTRIES` entries each; entries expire after `DEFAULT_TTL` and are
//! invalidated by path — every tool that writes a file calls
//! [`invalidate_path`], which drops scans whose root contains the changed path.
//! Owner: the process-wide [`shared`] singleton; nothing else holds scans, and
//! expiry keeps the map bounded even without explicit cleanup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use tokio_util::sync::CancellationToken;

use crate::ToolError;
use crate::builtin::helpers::canonicalize_best_effort;

/// How long a scan stays fresh. Short enough that out-of-band changes
/// (e.g. files created by a shell command) surface quickly; the write tools
/// invalidate instantly anyway.
pub const DEFAULT_TTL: Duration = Duration::from_secs(5);
/// Distinct (root, options) scans kept at once.
pub const MAX_ROOTS: usize = 16;
/// A scan larger than this is served but never stored — a huge tree would
/// dominate the cache for everyone else.
pub const MAX_SCAN_ENTRIES: usize = 32_768;

/// The walk options that differ between tools; everything else is uniform
/// (`git_ignore(true)`, `require_git(false)`, links unfollowed) and stays out
/// of the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WalkOptions {
    /// `true` includes dotfiles (`ignore::WalkBuilder::hidden` negated).
    pub include_hidden: bool,
    /// `ignore::WalkBuilder::max_depth`; `None` = unlimited.
    pub max_depth: Option<usize>,
}

/// One visited entry; `rel` is relative to the canonical walk root.
///
/// Not `Clone` on purpose: entries are only moved or borrowed (see
/// `join_parts`), so a deep copy of a whole scan cannot slip in.
#[derive(Debug)]
pub struct WalkEntry {
    pub rel: PathBuf,
    pub is_file: bool,
    pub is_dir: bool,
    /// Modification time for mtime sorting (`glob`); `None` when stat fails.
    pub mtime: Option<SystemTime>,
    /// Byte size (`ls`); 0 for directories and stat failures.
    pub size: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct WalkKey {
    root: PathBuf,
    options: WalkOptions,
}

struct Scan {
    created: Instant,
    entries: std::sync::Arc<Vec<WalkEntry>>,
}

/// Bounded TTL cache of directory scans. `collect` is blocking filesystem work;
/// callers run it inside `tokio::task::spawn_blocking`.
pub struct WalkCache {
    scans: Mutex<HashMap<WalkKey, Scan>>,
    ttl: Duration,
    max_roots: usize,
    max_scan_entries: usize,
}

impl WalkCache {
    pub fn new(ttl: Duration, max_roots: usize, max_scan_entries: usize) -> Self {
        Self {
            scans: Mutex::new(HashMap::new()),
            ttl,
            max_roots,
            max_scan_entries,
        }
    }

    /// Cached scan of `root`, or a fresh walk on a miss/expiry. `root` is
    /// canonicalized best-effort so `src` and `./src/../src` share one entry.
    pub fn collect(
        &self,
        root: &Path,
        options: WalkOptions,
        cancel: &CancellationToken,
    ) -> Result<std::sync::Arc<Vec<WalkEntry>>, ToolError> {
        let canonical_root = canonicalize_best_effort(root);
        let key = WalkKey {
            root: canonical_root.clone(),
            options,
        };
        {
            let scans = self.scans.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(scan) = scans.get(&key)
                && scan.created.elapsed() < self.ttl
            {
                return Ok(std::sync::Arc::clone(&scan.entries));
            }
        }

        let entries = std::sync::Arc::new(scan(&canonical_root, options, cancel)?);
        if entries.len() <= self.max_scan_entries {
            let mut scans = self.scans.lock().unwrap_or_else(|p| p.into_inner());
            scans.insert(
                key,
                Scan {
                    created: Instant::now(),
                    entries: std::sync::Arc::clone(&entries),
                },
            );
            evict_oldest(&mut scans, self.max_roots);
        }
        Ok(entries)
    }

    /// Drop every scan whose canonical root contains `target` — called after a
    /// file is written, renamed or deleted so the next walk sees it.
    pub fn invalidate_path(&self, target: &Path) {
        let target = canonicalize_best_effort(target);
        self.scans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|key, _| !target.starts_with(&key.root));
    }

    pub fn clear(&self) {
        self.scans.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub fn len(&self) -> usize {
        self.scans.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The process-wide cache shared by all tools and sessions.
pub fn shared() -> &'static WalkCache {
    static CACHE: OnceLock<WalkCache> = OnceLock::new();
    CACHE.get_or_init(|| WalkCache::new(DEFAULT_TTL, MAX_ROOTS, MAX_SCAN_ENTRIES))
}

/// Called by every tool that writes, moves or deletes a workspace file.
pub fn invalidate_path(target: &Path) {
    shared().invalidate_path(target);
}

fn evict_oldest(scans: &mut HashMap<WalkKey, Scan>, max_roots: usize) {
    while scans.len() > max_roots {
        let Some(oldest) = scans
            .iter()
            .min_by_key(|(_, scan)| scan.created)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        scans.remove(&oldest);
    }
}

/// Ceiling on walker threads for one unbounded scan, before the process-wide
/// budget (`crate::threads`) has its say.
const MAX_WALK_THREADS: usize = 8;

/// One `ignore`-crate walk producing plain entries sorted by relative path;
/// the root itself (depth 0) is never yielded since no consumer lists it.
///
/// An unbounded walk runs on `ignore`'s parallel walker when the process-wide
/// thread budget grants at least two threads: the cost is CPU in gitignore
/// matching, not IO, and on a large tree it dominates a cold `grep`/`glob`.
/// Otherwise — a depth-limited walk (`ls`, a single directory read) or a busy
/// budget — it walks serially on the calling thread. Either way the result is
/// sorted, so the order does not depend on thread timing or on `readdir`.
fn scan(
    root: &Path,
    options: WalkOptions,
    cancel: &CancellationToken,
) -> Result<Vec<WalkEntry>, ToolError> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(!options.include_hidden)
        .git_ignore(true)
        .require_git(false)
        .max_depth(options.max_depth);

    // The calling thread only waits while `ignore`'s threads walk, so fewer
    // than two extra threads would not be faster than walking here.
    let threads = match options.max_depth {
        Some(_) => None,
        None => Some(crate::threads::shared().reserve(MAX_WALK_THREADS)),
    }
    .filter(|reserved| reserved.count() >= 2);

    let mut entries = match threads {
        None => {
            let mut entries = Vec::new();
            for entry in builder.build() {
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                if let Some(entry) = walk_entry(root, entry) {
                    entries.push(entry);
                }
            }
            entries
        }
        Some(reserved) => {
            // Each walker thread fills its own Vec and hands it over when
            // its visitor is dropped at the end of the walk; the parts are
            // then moved into one Vec (`join_parts`), never cloned.
            let parts: Mutex<Vec<Vec<WalkEntry>>> = Mutex::new(Vec::new());
            builder.threads(reserved.count()).build_parallel().run(|| {
                let mut local = LocalEntries {
                    entries: Vec::new(),
                    parts: &parts,
                };
                Box::new(move |entry| {
                    if cancel.is_cancelled() {
                        return ignore::WalkState::Quit;
                    }
                    if let Some(entry) = walk_entry(root, entry) {
                        local.entries.push(entry);
                    }
                    ignore::WalkState::Continue
                })
            });
            drop(reserved);
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            join_parts(parts.into_inner().unwrap_or_else(|p| p.into_inner()))
        }
    };
    entries.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    Ok(entries)
}

/// The walker threads' parts as one Vec: the largest part is the base and the
/// others are moved onto it, so each entry's heap data moves rather than being
/// copied (`WalkEntry` is deliberately not `Clone`; `concat` would deep-copy).
/// Peak is the parts plus one buffer of entry headers for the result.
fn join_parts(mut parts: Vec<Vec<WalkEntry>>) -> Vec<WalkEntry> {
    let total: usize = parts.iter().map(Vec::len).sum();
    let Some(largest) = (0..parts.len()).max_by_key(|&i| parts[i].len()) else {
        return Vec::new();
    };
    let mut joined = parts.swap_remove(largest);
    joined.reserve_exact(total - joined.len());
    for mut part in parts {
        joined.append(&mut part);
    }
    joined
}

/// One walker thread's entries, appended to the shared parts on drop.
struct LocalEntries<'a> {
    entries: Vec<WalkEntry>,
    parts: &'a Mutex<Vec<Vec<WalkEntry>>>,
}

impl Drop for LocalEntries<'_> {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        self.parts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(entries);
    }
}

fn walk_entry(root: &Path, entry: Result<ignore::DirEntry, ignore::Error>) -> Option<WalkEntry> {
    let entry = entry.ok()?;
    if entry.depth() == 0 {
        return None;
    }
    // One stat per entry. Measured: spread over the walker threads it costs
    // little, and caching it keeps a warm `glob` (mtime sort) from re-stating
    // every match on each call.
    let file_type = entry.file_type();
    let (mtime, size) = match entry.metadata() {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    };
    let rel = entry
        .path()
        .strip_prefix(root)
        .unwrap_or_else(|_| entry.path())
        .to_path_buf();
    Some(WalkEntry {
        rel,
        is_file: file_type.is_some_and(|ft| ft.is_file()),
        is_dir: file_type.is_some_and(|ft| ft.is_dir()),
        mtime,
        size,
    })
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod walk_tests;
