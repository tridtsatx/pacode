use std::path::Path;
use std::time::Instant;

use pacode_types::TaskId;

use super::*;

const MEMINFO: &str = "MemTotal:       16384000 kB
MemFree:          500000 kB
MemAvailable:    8192000 kB
Buffers:          100000 kB
Cached:          2000000 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";

#[test]
fn meminfo_uses_mem_available_not_free() {
    let m = parse_meminfo(MEMINFO).expect("parses");
    assert_eq!(m.total, 16_384_000 * 1024);
    assert_eq!(m.available, 8_192_000 * 1024);
    assert_eq!(m.swap_total - m.swap_free, 1_048_576 * 1024);
}

#[test]
fn meminfo_without_mem_available_falls_back() {
    let text = "MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 25 kB\n";
    let m = parse_meminfo(text).expect("parses");
    assert_eq!(m.available, 175 * 1024);
    assert_eq!(parse_meminfo("garbage"), None);
}

#[test]
fn statm_rss_is_second_field_in_pages() {
    assert_eq!(
        parse_statm_rss("1000 250 100 10 0 300 0\n", 4096),
        Some(250 * 4096)
    );
    assert_eq!(parse_statm_rss("1000", 4096), None);
}

#[test]
fn proc_stat_handles_odd_command_names() {
    let line = "1234 (my (weird) cmd) S 99 1234 1234 0 -1 4194560 1";
    assert_eq!(
        parse_proc_stat(line),
        Some(ProcStat {
            ppid: 99,
            pgrp: 1234
        })
    );
    assert_eq!(parse_proc_stat("nonsense"), None);
}

#[test]
fn cgroup_limit_and_stat_parsers() {
    assert_eq!(parse_cgroup_limit("max\n"), None);
    assert_eq!(parse_cgroup_limit("1073741824\n"), Some(1_073_741_824));
    let stat = "anon 100\ninactive_file 4096\nactive_file 8\n";
    assert_eq!(parse_stat_field(stat, "inactive_file"), Some(4096));
    assert_eq!(parse_stat_field(stat, "missing"), None);
}

#[test]
fn proc_cgroup_v2_and_v1_lines() {
    let v2 = parse_proc_cgroup("0::/user.slice/app.scope\n");
    assert_eq!(v2.v2.as_deref(), Some("/user.slice/app.scope"));
    assert_eq!(v2.v1_memory, None);
    let v1 = parse_proc_cgroup("12:cpu,cpuacct:/a\n4:memory:/docker/abc\n");
    assert_eq!(v1.v2, None);
    assert_eq!(v1.v1_memory.as_deref(), Some("/docker/abc"));
}

#[test]
fn cgroup_limit_below_host_total_is_the_effective_total() {
    let mem = MemInfo {
        total: 28 << 30,
        available: 20 << 30,
        swap_total: 0,
        swap_free: 0,
    };
    let cg = CgroupMem {
        limit: 8 << 30,
        used: 6 << 30,
    };
    assert_eq!(effective_memory(&mem, Some(cg)), (8 << 30, 2 << 30));
    // A limit above host memory (or none) changes nothing.
    let big = CgroupMem {
        limit: 64 << 30,
        used: 1,
    };
    assert_eq!(effective_memory(&mem, Some(big)), (28 << 30, 20 << 30));
    assert_eq!(effective_memory(&mem, None), (28 << 30, 20 << 30));
}

#[test]
fn cgroup_ancestors_walk_up_to_the_root() {
    let dirs = ancestors_to(Path::new("/sys/fs/cgroup"), "/a/b");
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/sys/fs/cgroup/a/b"),
            PathBuf::from("/sys/fs/cgroup/a"),
            PathBuf::from("/sys/fs/cgroup"),
        ]
    );
    assert_eq!(
        ancestors_to(Path::new("/sys/fs/cgroup"), "/"),
        vec![PathBuf::from("/sys/fs/cgroup")]
    );
}

#[test]
fn real_proc_smoke() {
    let s = LinuxSampler::new();
    if !s.supported() {
        return;
    }
    let tmp = std::env::temp_dir();
    let host = s.host(&tmp).expect("host sample");
    assert!(host.total > 0 && host.available <= host.total);
    let root = TreeRoot {
        id: TaskId::new("self"),
        pgid: std::process::id(),
        started: Instant::now(),
        protected: false,
    };
    let trees = s.trees(&[root]);
    assert_eq!(trees.len(), 1);
    assert!(trees[0].rss > 0);
}
