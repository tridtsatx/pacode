use super::*;

#[cfg(unix)]
fn current_nofile() -> libc::rlimit {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `rl` is a valid, writable `rlimit` for the duration of the call.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) };
    assert_eq!(rc, 0, "getrlimit should succeed");
    rl
}

#[cfg(unix)]
#[test]
fn raise_nofile_is_monotonic() {
    let (old, new) = raise_nofile();
    assert!(new >= old, "soft limit must never decrease: {old} -> {new}");
    let rl = current_nofile();
    let hard = rl.rlim_max;
    assert!(new <= hard, "soft {new} must not exceed hard {hard}");
}

#[cfg(unix)]
#[test]
fn raise_nofile_second_call_is_noop() {
    let (_, first_new) = raise_nofile();
    let second = raise_nofile();
    assert_eq!(second, (first_new, first_new));
}

#[cfg(unix)]
#[test]
fn desired_soft_policy() {
    // Never lowers an already larger soft limit.
    assert_eq!(desired_soft(2_000_000, 4_000_000), 2_000_000);
    // Capped by the hard limit when that is below the target.
    assert_eq!(desired_soft(256, 1_000), 1_000);
    // Capped by the target when the hard limit is larger.
    assert_eq!(desired_soft(1_024, 1 << 40), NOFILE_TARGET);
}
