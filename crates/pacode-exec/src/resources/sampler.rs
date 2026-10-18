//! Sampling contract of the resource guard: plain data samples plus the
//! [`ResourceSampler`] trait, so the policy and the guard are testable with a fake.
//!
//! Cost model: samples are only taken while at least one supervised process is
//! alive (see `guard.rs`); nothing here owns a timer or a thread.

use std::path::Path;
use std::time::Instant;

use pacode_types::TaskId;

/// Host-wide numbers, already reduced to the *effective* limits: when a cgroup
/// limit is lower than host memory, `total`/`available` describe the cgroup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostSample {
    /// Effective total memory in bytes: `min(host total, cgroup max)`.
    pub total: u64,
    /// Memory that can be handed out without swapping (`MemAvailable`, or the
    /// cgroup headroom when that is smaller).
    pub available: u64,
    /// Swap currently in use, in bytes (informational).
    pub swap_used: u64,
    /// Free bytes on the spool directory's filesystem, when known.
    pub disk_free: Option<u64>,
}

/// A supervised process tree as the manager knows it: the leader is also the
/// process-group id (every task is spawned with `process_group(0)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRoot {
    pub id: TaskId,
    pub pgid: u32,
    pub started: Instant,
    /// Never chosen as a victim (the guard still reports its memory).
    pub protected: bool,
}

/// Memory of one supervised tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSample {
    pub id: TaskId,
    /// Sum of resident set sizes over the tree, in bytes.
    pub rss: u64,
    pub started: Instant,
    pub protected: bool,
}

/// Source of samples. Implementations may block briefly (reading `/proc`); the guard
/// calls them from `spawn_blocking`.
pub trait ResourceSampler: Send + Sync {
    /// False on platforms where nothing can be measured: the guard then disables
    /// itself and says so once.
    fn supported(&self) -> bool;

    /// Host numbers, or `None` when they cannot be read right now.
    fn host(&self, spool_dir: &Path) -> Option<HostSample>;

    /// Memory of each root's tree. A root whose process vanished is omitted.
    fn trees(&self, roots: &[TreeRoot]) -> Vec<TreeSample>;
}

/// Free bytes available to unprivileged writers on the filesystem holding `path`.
/// Walks up to the nearest existing ancestor (the spool dir may not exist yet).
#[cfg(unix)]
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.exists() {
            let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
            // SAFETY: `st` is plain-old-data and `c` is a valid NUL-terminated path.
            let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
            if rc != 0 {
                return None;
            }
            #[allow(clippy::unnecessary_cast)]
            let free = (st.f_bavail as u64).saturating_mul(st.f_frsize as u64);
            return Some(free);
        }
        cur = p.parent();
    }
    None
}

#[cfg(not(unix))]
pub fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}
