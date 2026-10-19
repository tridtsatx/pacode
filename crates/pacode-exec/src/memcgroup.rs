//! The preferred enforcement: a cgroup v2 subtree this daemon owns, one leaf
//! per session, `memory.max` as the limit.
//!
//! # Why this is the mechanism the feature wants
//!
//! - It counts **resident** pages. A JVM that reserves 8 GiB of address space
//!   and touches 300 MiB is charged 300 MiB, which is the number a user means
//!   by "memory".
//! - The kernel enforces it: the allocation fails or the process is reclaimed
//!   by the cgroup's own OOM killer. Nothing can opt out, not even a script.
//! - `memory.max` is a file, so a raise is a write to a path, on a process
//!   that is already running. No re-exec, no privilege, no cooperation from
//!   the process.
//!
//! # The layout
//!
//! ```text
//! /sys/fs/cgroup/<our own cgroup>/pacode-exec-<daemon pid>/   <- scope
//!                                     memory.max = the undivided machine ceiling
//!                                     cgroup.subtree_control = "+memory"
//!                                     s-pacode-1a2b3c4d-0001/   <- one per session
//!                                         memory.max = the session's share
//!                                         cgroup.procs = the tool processes
//! ```
//!
//! Two levels, and the level above is not decoration. The scope is capped at
//! the *undivided* machine ceiling, so even if the session split were wrong,
//! the sum of everything this daemon spawns stays inside the ceiling the user
//! configured. Sessions divide that budget among themselves below it. A
//! granted raise is clamped to the scope's ceiling for the same reason: one
//! session asking for more may not spend money the other sessions are relying
//! on.
//!
//! # What is left behind when the daemon dies
//!
//! The scope directory, named with the pid that made it, if the daemon was
//! killed rather than shut down. It is inert — no `memory.max` survives a
//! reboot, and a scope with no live processes holds nothing — and it is
//! identifiable by its dead pid, so it is sweepable. The *division* is what
//! mattered and that lives only in RAM ([`crate::memsessions`]), so a crash
//! cannot leave stale shares behind.

use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};

use pacode_types::SessionId;

use crate::manager::ExecError;
use crate::memcap::{LimitKind, MemoryCeiling, MemoryPolicy};
use crate::memlimit::{LimitBackend, RaiseTarget, SpawnSetup};

/// Why cgroup v2 cannot be used here. Reported once, at startup.
#[derive(Debug, thiserror::Error)]
pub enum CgroupUnavailable {
    #[error("no cgroup v2 unified hierarchy at /sys/fs/cgroup")]
    NoUnifiedHierarchy,
    #[error("the cgroup v2 memory controller is not available")]
    NoMemoryController,
    #[error("the daemon is in the root cgroup, where a private subtree cannot be made")]
    InRootCgroup,
    #[error("cgroup v2 is unified but the daemon is not in a delegated cgroup")]
    NoOwnCgroup,
    #[error("cannot create or control a cgroup subtree: {0}")]
    NotPermitted(std::io::Error),
}

/// A failed write to a cgroup control file. The path is carried along because
/// the cgroup filesystem reports almost nothing useful: "No such file or
/// directory" is a deleted group, a moved process, or the wrong mount, and
/// only the path tells them apart.
#[derive(Debug)]
pub enum LimitWriteError {
    Io(PathBuf, std::io::Error),
}

impl LimitWriteError {
    fn into_io(self) -> std::io::Error {
        match self {
            Self::Io(path, source) => std::io::Error::new(
                source.kind(),
                format!("cgroup {}: {source}", path.display()),
            ),
        }
    }
}

impl From<LimitWriteError> for ExecError {
    fn from(error: LimitWriteError) -> Self {
        ExecError::MemoryLimit(error.into_io())
    }
}

/// A daemon-owned cgroup v2 subtree.
#[derive(Debug)]
pub struct CgroupV2 {
    scope: PathBuf,
    /// The undivided machine ceiling, written to the scope. Also the clamp for
    /// any granted raise.
    scope_max: u64,
    /// False when `memory` could not be enabled in the scope's
    /// `cgroup.subtree_control`, so session leaves cannot carry their own
    /// `memory.max` and only the scope-level cap binds.
    per_session: bool,
    /// `(session, limit)` pairs already written. A session's share only changes
    /// when the number of live sessions changes, so re-writing the same
    /// `memory.max` on every spawn is a syscall per tool for a value that did
    /// not move — and under load that is exactly what a tool spawn must not
    /// pay. The entry doubles as proof the directory exists, which saves the
    /// `create_dir` on every spawn after the first.
    written: std::sync::Mutex<std::collections::HashMap<pacode_types::SessionId, u64>>,
}

impl CgroupV2 {
    pub fn scope(&self) -> &Path {
        &self.scope
    }

    /// Build a subtree, or say why not. Four file operations at most; the
    /// result is reused for the life of the daemon and never re-probed.
    pub fn probe(policy: &MemoryPolicy) -> Result<Self, CgroupUnavailable> {
        let controllers = fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
            .map_err(|_| CgroupUnavailable::NoUnifiedHierarchy)?;
        if !controller_enabled(&controllers, "memory") {
            return Err(CgroupUnavailable::NoMemoryController);
        }
        let own = own_cgroup_path()?;
        let base = Path::new("/sys/fs/cgroup").join(own);
        if !base.is_dir() {
            return Err(CgroupUnavailable::NoOwnCgroup);
        }
        // The child inherits controllers only if the parent hands them down,
        // and cgroup v2 refuses to enable a controller in a cgroup that still
        // has processes in it. Both failures mean "no subtree for us here".
        enable_controller(&base.join("cgroup.subtree_control"), "memory")
            .map_err(CgroupUnavailable::NotPermitted)?;

        let scope = base.join(format!("pacode-exec-{}", std::process::id()));
        create_dir(&scope).map_err(CgroupUnavailable::NotPermitted)?;

        if !scope.join("memory.max").exists() {
            let _ = fs::remove_dir(&scope);
            return Err(CgroupUnavailable::NoMemoryController);
        }
        let per_session =
            enable_controller(&scope.join("cgroup.subtree_control"), "memory").is_ok();

        let cgroup = Self {
            scope,
            scope_max: policy.ceiling_for(1).unwrap_or(policy.machine_ram_bytes),
            per_session,
            written: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        if let Err(error) = cgroup.write_limit(&cgroup.scope, cgroup.scope_max) {
            let _ = fs::remove_dir(&cgroup.scope);
            return Err(CgroupUnavailable::NotPermitted(error.into_io()));
        }
        log::info!(
            "cgroup v2 tool memory: scope {}, ceiling {}, per-session {}",
            cgroup.scope.display(),
            cgroup.scope_max,
            if cgroup.per_session {
                "groups"
            } else {
                "unavailable"
            },
        );
        Ok(cgroup)
    }

    fn group_dir(&self, session: &SessionId) -> PathBuf {
        self.scope.join(group_name(session))
    }

    /// Create the session's leaf and cap it, or confirm the two are already in
    /// place. Idempotent, and *cheap* when they are: a session that reopens
    /// keeps the same group and the same pid-independent limit, and a spawn
    /// whose share has not moved must not pay a `mkdir` and a file write for it.
    fn ensure_group(
        &self,
        session: &SessionId,
        ceiling: &MemoryCeiling,
    ) -> Result<PathBuf, ExecError> {
        let dir = self.group_dir(session);
        let settled = self
            .written
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session)
            .is_some_and(|written| !self.per_session || *written == ceiling.bytes);
        if settled {
            return Ok(dir);
        }
        create_dir(&dir).map_err(ExecError::MemoryLimit)?;
        if self.per_session {
            self.write_limit(&dir, ceiling.bytes)?;
            self.written
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(session.clone(), ceiling.bytes);
        }
        Ok(dir)
    }

    fn write_limit(&self, dir: &Path, bytes: u64) -> Result<(), LimitWriteError> {
        write_file(&dir.join("memory.max"), &bytes.to_string())
    }
}

impl Drop for CgroupV2 {
    fn drop(&mut self) {
        // Hand the memory back before removing the directory, so a sweep that
        // succeeds cannot race a raise.
        let _ = write_file(&self.scope.join("memory.max"), "max");
        if let Err(error) = fs::remove_dir(&self.scope) {
            log::debug!(
                "cgroup v2 scope {} not removed ({error}); it is inert and named for a dead pid",
                self.scope.display()
            );
        }
    }
}

impl LimitBackend for CgroupV2 {
    fn kind(&self) -> LimitKind {
        LimitKind::Cgroup
    }

    fn session_opened(&self, session: &SessionId, ceiling: &MemoryCeiling) {
        if let Err(error) = self.ensure_group(session, ceiling) {
            log::warn!("tool memory: no cgroup for session {session}: {error}");
        }
    }

    fn session_closed(&self, session: &SessionId) {
        let dir = self.group_dir(session);
        // Release the share first: a group that outlives its session must not
        // keep the memory the next session now expects to have.
        let _ = write_file(&dir.join("memory.max"), "max");
        if let Err(error) = fs::remove_dir(&dir) {
            log::debug!(
                "cgroup v2 group {} not removed ({error}); a task is probably still in it",
                dir.display()
            );
        }
    }

    fn apply_at_spawn(
        &self,
        session: &SessionId,
        ceiling: &MemoryCeiling,
    ) -> Result<SpawnSetup, ExecError> {
        let dir = self.ensure_group(session, ceiling)?;
        let procs = dir.join("cgroup.procs");
        let procs = CString::new(procs.as_os_str().as_encoded_bytes()).map_err(|_| {
            ExecError::MemoryLimit(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("cgroup path is not usable: {}", procs.display()),
            ))
        })?;
        Ok(SpawnSetup::CgroupJoin { procs })
    }

    fn raise(&self, target: &RaiseTarget) -> Result<(), ExecError> {
        let dir = self.group_dir(&target.session);
        if !dir.is_dir() {
            return Err(ExecError::MemoryLimit(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no cgroup for session {}", target.session),
            )));
        }
        // A raise may not spend the shares the other live sessions hold.
        self.write_limit(&dir, clamp_to_scope(target.bytes, self.scope_max))?;
        Ok(())
    }
}

/// `cgroup.subtree_control` is a space-separated list; `cgroup.controllers` is
/// the list available to be enabled. Parsed rather than string-searched so a
/// controller named `memory_recursive` cannot be mistaken for `memory`.
pub(crate) fn controller_enabled(list: &str, name: &str) -> bool {
    list.split_whitespace().any(|entry| entry == name)
}

/// A granted raise may not spend the memory the other live sessions are
/// relying on: the scope above them is the undivided machine ceiling, and a
/// session that could write past it would turn a split limit back into an
/// unlimited one.
pub(crate) fn clamp_to_scope(bytes: u64, scope_max: u64) -> u64 {
    bytes.min(scope_max)
}

/// The daemon's own cgroup, relative to the unified mount. The root cgroup is
/// rejected: it holds every process on the machine, so no memory controller
/// can be enabled for a subtree below it.
fn own_cgroup_path() -> Result<PathBuf, CgroupUnavailable> {
    let raw =
        fs::read_to_string("/proc/self/cgroup").map_err(|_| CgroupUnavailable::NoOwnCgroup)?;
    let line = raw
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or(CgroupUnavailable::NoOwnCgroup)?;
    let rel = line.trim();
    if rel.is_empty() {
        return Err(CgroupUnavailable::NoOwnCgroup);
    }
    if rel == "/" {
        return Err(CgroupUnavailable::InRootCgroup);
    }
    Ok(PathBuf::from(rel.trim_start_matches('/')))
}

fn create_dir(path: &Path) -> std::io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Already there is success: two sessions can race, and a reopen is
        // normal.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn enable_controller(file: &Path, name: &str) -> std::io::Result<()> {
    let current = fs::read_to_string(file)?;
    if controller_enabled(&current, name) {
        return Ok(());
    }
    fs::write(file, format!("+{name}"))
}

fn write_file(path: &Path, contents: &str) -> Result<(), LimitWriteError> {
    fs::write(path, contents).map_err(|error| LimitWriteError::Io(path.to_path_buf(), error))
}

/// Filesystem-safe group name for a session id. Session ids are generated as
/// `pacode-<hex>`, but an id can be supplied by a client, so anything outside
/// the safe set is escaped rather than trusted to stay inside the scope.
pub(crate) fn group_name(session: &SessionId) -> String {
    let raw = session.as_str();
    let safe = !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !raw.starts_with('.');
    if safe {
        format!("s-{raw}")
    } else {
        format!("s-{}", escape(raw))
    }
}

fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            out.push(byte as char);
        } else {
            out.push('_');
        }
    }
    // Length is bounded so a long id cannot blow past NAME_MAX, and the tail
    // keeps two ids that escape alike from colliding.
    out.truncate(40);
    out.push('-');
    out.push_str(&format!("{:x}", fnv1a(raw.as_bytes())));
    out
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[path = "memcgroup_tests.rs"]
mod memcgroup_tests;
