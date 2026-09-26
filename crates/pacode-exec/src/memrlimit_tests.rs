use pacode_types::SessionId;

use super::*;
use crate::memcap::MemoryPolicy;

const GIB: u64 = 1024 * 1024 * 1024;

#[cfg(target_os = "linux")]
fn limit_of(pid: u32) -> (u64, u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid out-parameter. Reading another process's
    // limit needs no privilege; only raising it past the hard limit does.
    let rc = unsafe {
        libc::prlimit(
            pid as libc::pid_t,
            libc::RLIMIT_AS,
            std::ptr::null(),
            &mut limit,
        )
    };
    assert_eq!(rc, 0, "prlimit(get) failed");
    (limit.rlim_cur, limit.rlim_max)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_soft_limit_is_the_ceiling_and_the_hard_limit_is_the_machine() {
    let soft = 2 * GIB;
    let hard = 8 * GIB;
    let setup = RlimitBackend::new();
    let ceiling = MemoryCeiling {
        bytes: soft,
        hard_max_bytes: hard,
        kind: setup.kind(),
        live_sessions: 1,
        percent: 80,
        split_sessions: true,
        machine_ram_bytes: hard,
    };
    let plan = setup
        .apply_at_spawn(&SessionId::new("probe"), &ceiling)
        .expect("rlimit arms");

    // Run the setup exactly the way the task manager does — between fork and
    // exec — and read the numbers back out of the child, so a swapped pair
    // cannot pass.
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("ulimit -Sv; ulimit -Hv")
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: `SpawnSetup::install` is bare syscalls on memory allocated
    // before the fork, which is the contract `pre_exec` requires.
    unsafe { command.pre_exec(move || plan.install()) };
    let output = command.output().await.expect("child runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // `ulimit -v` is reported in kibibytes, one line per limit.
    let reported = String::from_utf8(output.stdout).expect("ascii");
    let mut lines = reported.lines();
    let soft_kb: u64 = lines
        .next()
        .expect("a soft limit")
        .trim()
        .parse()
        .expect("a numeric soft limit");
    let hard_kb: u64 = lines
        .next()
        .expect("a hard limit")
        .trim()
        .parse()
        .expect("a numeric hard limit");
    assert_eq!(soft_kb * 1024, soft, "the soft limit is the ceiling");
    assert_eq!(hard_kb * 1024, hard, "the hard limit is the machine");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_live_process_is_raised_without_privileges_or_a_re_exec() {
    // The point of putting the hard limit at the machine's RAM rather than at
    // the ceiling: an unprivileged `prlimit` can move the soft limit anywhere
    // up to it, on a process that is already running.
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .kill_on_drop(true)
        .spawn()
        .expect("child spawns");
    let pid = child.id().expect("child has a pid");

    raise_pid(pid, 2 * GIB, 8 * GIB).expect("prlimit moves a live process");
    assert_eq!(limit_of(pid), (2 * GIB, 8 * GIB));

    // A raise can move the soft limit, but it cannot lift the hard limit
    // further: only a process with CAP_SYS_RESOURCE may do that, and the hard
    // limit is what stops a grant from exceeding the machine. Root is exempt
    // from that rule, so the check only means anything unprivileged.
    if unsafe { libc::geteuid() } != 0 {
        assert!(
            raise_pid(pid, 2 * GIB, 16 * GIB).is_err(),
            "lifting the hard limit must need a privilege"
        );
        assert_eq!(limit_of(pid), (2 * GIB, 8 * GIB));
    }

    child.start_kill().ok();
    child.wait().await.ok();
}

#[test]
fn the_machine_size_is_read_from_the_kernel_or_not_at_all() {
    match machine_ram_bytes() {
        Some(bytes) => {
            assert!(bytes > 0);
            // A machine with 64 PiB of RAM is not a machine.
            assert!(bytes < 1 << 46, "implausible RAM: {bytes}");
            let policy = MemoryPolicy {
                percent: 80,
                split_sessions: true,
                machine_ram_bytes: bytes,
            };
            assert!(policy.is_enabled());
        }
        None => {
            // No size, no cap: a percentage of an unknown machine is not a
            // number, and guessing would be a limit nobody could debug.
            let policy = MemoryPolicy {
                percent: 80,
                split_sessions: true,
                machine_ram_bytes: 0,
            };
            assert!(!policy.is_enabled());
            assert_eq!(policy.ceiling_for(1), None);
        }
    }
}
