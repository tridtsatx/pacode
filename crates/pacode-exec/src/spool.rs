//! Spool directory garbage collection (F5).
//!
//! Each task writes `<spool_dir>/<task_id>.out` (at most `exec.max_spool_bytes`), and
//! nothing used to delete them. [`gc`] keeps the directory under a total byte cap by
//! deleting the oldest files first.
//!
//! - Owner: `TaskManager` (run at construction and after every 64th terminal eviction;
//!   no timer, nothing ticks while idle).
//! - Cap: [`SPOOL_MAX_TOTAL_BYTES`] (1 GiB) over all `*.out` files.
//! - Invalidation: none; each run re-reads the directory.
//! - A deleted spool means `TaskManager::tail`/`output` of that old task return an empty
//!   result (documented behaviour); `TaskInfo::output_path` then points at a missing file.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use pacode_types::TaskId;

/// Total size of `*.out` spool files kept on disk.
pub(crate) const SPOOL_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// Delete the oldest `*.out` files in `dir` until the total is at most
/// `max_total_bytes`. Files whose stem is in `keep` (running tasks) are never deleted.
/// Returns the number of files removed. Blocking: call from `spawn_blocking`.
pub(crate) fn gc(dir: &Path, max_total_bytes: u64, keep: &HashSet<TaskId>) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(SystemTime, u64, std::path::PathBuf, bool)> = Vec::new();
    let mut total: u64 = 0;
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("out") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let protected = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| keep.contains(&TaskId::new(stem)));
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total = total.saturating_add(meta.len());
        files.push((modified, meta.len(), path, protected));
    }
    if total <= max_total_bytes {
        return 0;
    }
    files.sort_by_key(|f| f.0);
    let mut removed = 0;
    for (_, len, path, protected) in files {
        if total <= max_total_bytes {
            break;
        }
        if protected {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
#[path = "spool_tests.rs"]
mod spool_tests;
