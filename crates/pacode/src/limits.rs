//! Process resource limits applied once at daemon start.

/// Soft `RLIMIT_NOFILE` target. The soft limit is raised toward this value, never past
/// the hard limit.
pub const NOFILE_TARGET: u64 = 65_536;

/// macOS refuses soft open-file limits above `OPEN_MAX` (10240) when the hard limit is
/// unlimited, so the target is clamped to it there.
#[cfg(target_os = "macos")]
const NOFILE_CAP_UNLIMITED_HARD: u64 = 10_240;

/// Raise the soft `RLIMIT_NOFILE` limit to `min(hard, NOFILE_TARGET)`.
///
/// The soft limit is never lowered: if it is already at or above the target, nothing is
/// changed. Returns `(old_soft, new_soft)`. If the limit cannot be read, a warning is
/// logged and `(0, 0)` is returned. If it cannot be set, a warning is logged and
/// `(old_soft, old_soft)` is returned, so startup continues either way.
#[cfg(unix)]
pub fn raise_nofile() -> (u64, u64) {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `rl` is a valid, writable `rlimit` for the duration of the call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 {
        log::warn!(
            "getrlimit(RLIMIT_NOFILE) failed: {}",
            std::io::Error::last_os_error()
        );
        return (0, 0);
    }

    let old = rl.rlim_cur;
    let hard = rl.rlim_max;
    let target = desired_soft(old, hard);
    if target == old {
        return (old, old);
    }

    rl.rlim_cur = target as libc::rlim_t;
    // SAFETY: `rl` is a valid `rlimit` and the pointer is only read by the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) } != 0 {
        log::warn!(
            "setrlimit(RLIMIT_NOFILE) to {target} failed, keeping {old}: {}",
            std::io::Error::last_os_error()
        );
        return (old, old);
    }
    (old, target)
}

/// Non-unix hosts have no `RLIMIT_NOFILE`; nothing is changed.
#[cfg(not(unix))]
pub fn raise_nofile() -> (u64, u64) {
    (0, 0)
}

/// Soft limit to request: the current soft limit or the capped hard limit, whichever is
/// larger. Pure so the policy can be tested without touching process limits.
#[cfg(unix)]
fn desired_soft(soft: u64, hard: u64) -> u64 {
    let ceiling = hard.min(NOFILE_TARGET);
    #[cfg(target_os = "macos")]
    let ceiling = if hard == libc::RLIM_INFINITY as u64 {
        NOFILE_CAP_UNLIMITED_HARD
    } else {
        ceiling
    };
    soft.max(ceiling)
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod limits_tests;
