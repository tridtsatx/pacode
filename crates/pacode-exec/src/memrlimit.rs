//! The fallback enforcement: `setrlimit(RLIMIT_AS)` in the child between fork
//! and exec, `prlimit` to raise a live process.
//!
//! # Why this is only the fallback
//!
//! cgroup v2 (see [`crate::memcgroup`]) is the mechanism this feature wants:
//! it counts resident pages, it is a property of the process rather than of
//! the address space, and `memory.max` is a writable file, so a raise is a
//! rewrite and never a re-exec. `RLIMIT_AS` has none of those properties. It
//! caps *virtual* address space, and a JVM, a Go runtime or Node reserves
//! address space it never touches: Go alone maps hundreds of MiB of PROT_NONE
//! arena up front, and V8 reserves a code range before running a line of
//! script. Under a ceiling a native program would not notice, every one of them
//! fails to start.
//!
//! # What this module does about that
//!
//! It applies the limit — **not silently**. Three things make the difference
//! between "a broken JVM" and "a JVM that told you why":
//!
//! 1. **The limit is soft.** The hard limit is the machine's RAM, and a process
//!    may raise its own soft limit up to the hard limit without any privilege
//!    and without a re-exec. A runtime that knows it needs more room can lift
//!    the ceiling itself; the ceiling is a default to be consciously overridden,
//!    not a wall. What actually protects the machine is the hard limit, and
//!    that is not waivable.
//! 2. **The limit is named.** `PACODE_MEMORY_LIMIT_KIND=address-space` plus the
//!    `PACODE_MEMORY_LIMIT` sentence go into the tool's environment, so a
//!    script and the model both know the number is an address-space ceiling
//!    and not "how much RAM I may touch".
//! 3. **A raise is available.** A tool that cannot start under the ceiling
//!    asks for a higher one ([`crate::memlimit::MemoryLimiter::request_raise`]),
//!    and that ask goes to the user even when action approval is bypassed.
//!
//! The alternative — detect managed runtimes and skip the cap for them — was
//! rejected: it trades a known, loud, diagnosable failure for an unbounded
//! allocation in exactly the case the cap exists for, and it is guesswork, since
//! the process that allocates is a grandchild three shells down.

use std::io;

use pacode_types::SessionId;

use crate::manager::ExecError;
use crate::memcap::{LimitKind, MemoryCeiling};
use crate::memlimit::{LimitBackend, RaiseTarget, SpawnSetup};

/// `RLIMIT_AS` backend. Holds nothing: the limit travels with the process, and
/// a raise addresses the process by pid.
#[derive(Debug, Default, Clone, Copy)]
pub struct RlimitBackend;

impl RlimitBackend {
    pub const fn new() -> Self {
        Self
    }
}

impl LimitBackend for RlimitBackend {
    fn kind(&self) -> LimitKind {
        LimitKind::AddressSpace
    }

    fn apply_at_spawn(
        &self,
        _session: &SessionId,
        ceiling: &MemoryCeiling,
    ) -> Result<SpawnSetup, ExecError> {
        Ok(SpawnSetup::RlimitAs {
            soft: ceiling.bytes,
            hard: ceiling.hard_max_bytes,
        })
    }

    fn raise(&self, target: &RaiseTarget) -> Result<(), ExecError> {
        let Some(pid) = target.pid else {
            return Err(ExecError::MemoryLimit(std::io::Error::new(
                io::ErrorKind::InvalidInput,
                "no pid to raise: RLIMIT_AS can only be changed on a known process",
            )));
        };
        raise_pid(pid, target.bytes, target.hard_max_bytes)
    }
}

/// `prlimit(pid, RLIMIT_AS, {bytes, hard})` on a process that is already
/// running. Legal without privilege precisely because the hard limit was set
/// to the machine's RAM when the process was execed: an unprivileged process
/// may raise its soft limit up to its hard limit, and no further.
#[cfg(target_os = "linux")]
pub fn raise_pid(pid: u32, bytes: u64, hard: u64) -> Result<(), ExecError> {
    let limit = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: `limit` is fully initialised and the pointers are valid for the
    // duration of the call; a null out-parameter means "do not report the old
    // value".
    let rc = unsafe {
        libc::prlimit(
            pid as libc::pid_t,
            libc::RLIMIT_AS,
            &limit,
            std::ptr::null_mut(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(ExecError::MemoryLimit(io::Error::last_os_error()))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn raise_pid(_pid: u32, _bytes: u64, _hard: u64) -> Result<(), ExecError> {
    Err(ExecError::MemoryLimit(io::Error::new(
        io::ErrorKind::Unsupported,
        "raising a live process limit needs Linux prlimit",
    )))
}

/// `setrlimit(RLIMIT_AS, {soft, hard})`, called in the child between fork and
/// exec. One syscall and no allocation, which is what makes it legal in a
/// `pre_exec` closure.
#[cfg(target_os = "linux")]
pub fn apply_in_child(soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: `limit` is fully initialised and the call is a bare syscall, so
    // nothing here can allocate, take a lock, or run Rust code.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn apply_in_child(_soft: u64, _hard: u64) -> io::Result<()> {
    Ok(())
}

/// Total machine RAM, or `None` when it cannot be read — in which case the
/// policy cannot compute a ceiling and the cap stays off rather than becoming
/// a guess.
pub fn machine_ram_bytes() -> Option<u64> {
    read_memtotal().or_else(sysctl_memsize)
}

#[cfg(target_os = "linux")]
fn read_memtotal() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        let Some(rest) = line.strip_prefix("MemTotal:") else {
            continue;
        };
        let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
        return Some(kb.saturating_mul(1024));
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_memtotal() -> Option<u64> {
    None
}

#[cfg(target_vendor = "apple")]
fn sysctl_memsize() -> Option<u64> {
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: `value`/`len` are a valid out-parameter pair for `sysctlbyname`
    // and the name is a NUL-terminated literal.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::from_mut(&mut value).cast(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    (rc == 0).then_some(value)
}

#[cfg(not(target_vendor = "apple"))]
fn sysctl_memsize() -> Option<u64> {
    None
}

#[cfg(test)]
#[path = "memrlimit_tests.rs"]
mod memrlimit_tests;
