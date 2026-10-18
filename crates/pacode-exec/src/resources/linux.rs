//! Linux sampler: `/proc/meminfo`, cgroup v2/v1 memory files and per-process
//! `/proc/<pid>/statm`. Parsers are pure functions over file contents so they can be
//! tested on fixture strings.
//!
//! Tree discovery cost: by default a breadth-first walk over
//! `/proc/<pid>/task/*/children` (one `read_dir` plus one small read per thread and
//! one `statm` read per process of the tree; nothing outside the tree is touched, and a
//! lone process without children costs two reads). When the kernel has no `children`
//! files (`CONFIG_PROC_CHILDREN` off) one scan of `/proc/*/stat` per sample is used
//! instead, matching the process group or any descendant through the ppid chain.
//! Each tree is capped at [`MAX_TREE_PIDS`] processes. RSS is summed per process, so
//! pages shared between forked workers are counted more than once: a deliberate
//! over-estimate, cheaper than `smaps_rollup` Pss.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::sampler::{HostSample, ResourceSampler, TreeRoot, TreeSample, disk_free_bytes};

const MAX_TREE_PIDS: usize = 4096;
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

/// Parse `/proc/meminfo` (values in kB). `MemAvailable` is preferred; kernels without
/// it fall back to `MemFree + Buffers + Cached`.
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut total = None;
    let mut avail = None;
    let (mut free, mut buffers, mut cached) = (0u64, 0u64, 0u64);
    let (mut swap_total, mut swap_free) = (0u64, 0u64);
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = kb.saturating_mul(1024);
        match key {
            "MemTotal" => total = Some(bytes),
            "MemAvailable" => avail = Some(bytes),
            "MemFree" => free = bytes,
            "Buffers" => buffers = bytes,
            "Cached" => cached = bytes,
            "SwapTotal" => swap_total = bytes,
            "SwapFree" => swap_free = bytes,
            _ => {}
        }
    }
    let total = total?;
    let available = avail.unwrap_or_else(|| free.saturating_add(buffers).saturating_add(cached));
    Some(MemInfo {
        total,
        available: available.min(total),
        swap_total,
        swap_free,
    })
}

/// Resident bytes from `/proc/<pid>/statm` (second field is resident pages).
pub fn parse_statm_rss(text: &str, page_size: u64) -> Option<u64> {
    let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages.saturating_mul(page_size))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcStat {
    pub ppid: u32,
    pub pgrp: u32,
}

/// Parse `/proc/<pid>/stat`. The command name sits in parentheses and may contain
/// spaces and parentheses, so fields are counted after the last `)`.
pub fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let rest = &text[text.rfind(')')? + 1..];
    let mut it = rest.split_whitespace();
    let _state = it.next()?;
    let ppid = it.next()?.parse().ok()?;
    let pgrp = it.next()?.parse().ok()?;
    Some(ProcStat { ppid, pgrp })
}

/// `memory.max` / `memory.limit_in_bytes`: a number, or `max` (unlimited) -> `None`.
pub fn parse_cgroup_limit(text: &str) -> Option<u64> {
    let t = text.trim();
    if t == "max" {
        return None;
    }
    t.parse().ok()
}

/// Value of `key` in a flat `memory.stat` file.
pub fn parse_stat_field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == key).then(|| v.trim().parse().ok()).flatten()
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CgroupPaths {
    /// Path of the unified (v2) hierarchy, e.g. `/user.slice/foo.scope`.
    pub v2: Option<String>,
    /// Path inside the v1 `memory` controller.
    pub v1_memory: Option<String>,
}

/// Parse `/proc/self/cgroup` (`hierarchy:controllers:path` lines).
pub fn parse_proc_cgroup(text: &str) -> CgroupPaths {
    let mut out = CgroupPaths::default();
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(hier), Some(ctrl), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if hier == "0" && ctrl.is_empty() {
            out.v2 = Some(path.to_string());
        } else if ctrl.split(',').any(|c| c == "memory") {
            out.v1_memory = Some(path.to_string());
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CgroupMem {
    pub limit: u64,
    /// Usage with reclaimable page cache (`inactive_file`) already subtracted.
    pub used: u64,
}

/// Reduce host and cgroup numbers to the effective `(total, available)`.
pub fn effective_memory(mem: &MemInfo, cg: Option<CgroupMem>) -> (u64, u64) {
    match cg {
        Some(cg) if cg.limit < mem.total => (
            cg.limit,
            mem.available.min(cg.limit.saturating_sub(cg.used)),
        ),
        _ => (mem.total, mem.available),
    }
}

/// Ancestors of `path` from itself up to (and including) `root`.
fn ancestors_to(root: &Path, rel: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut cur = root.join(rel.trim_start_matches('/'));
    while cur.starts_with(root) {
        dirs.push(cur.clone());
        if cur == root || !cur.pop() {
            break;
        }
    }
    if dirs.is_empty() {
        dirs.push(root.to_path_buf());
    }
    dirs
}

#[derive(Clone, Debug)]
struct CgroupDirs {
    v2: bool,
    dirs: Vec<PathBuf>,
}

fn resolve_cgroup_dirs() -> Option<CgroupDirs> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let paths = parse_proc_cgroup(&text);
    if let Some(p) = paths.v2 {
        return Some(CgroupDirs {
            v2: true,
            dirs: ancestors_to(Path::new(CGROUP_ROOT), &p),
        });
    }
    let p = paths.v1_memory?;
    Some(CgroupDirs {
        v2: false,
        dirs: ancestors_to(&Path::new(CGROUP_ROOT).join("memory"), &p),
    })
}

fn read_cgroup_mem(c: &CgroupDirs) -> Option<CgroupMem> {
    let (limit_file, cur_file, inactive_keys): (&str, &str, &[&str]) = if c.v2 {
        ("memory.max", "memory.current", &["inactive_file"])
    } else {
        (
            "memory.limit_in_bytes",
            "memory.usage_in_bytes",
            &["total_inactive_file", "inactive_file"],
        )
    };
    // Nearest enclosing cgroup that has a numeric limit.
    for dir in &c.dirs {
        let Some(limit) = std::fs::read_to_string(dir.join(limit_file))
            .ok()
            .and_then(|t| parse_cgroup_limit(&t))
        else {
            continue;
        };
        let current: u64 = std::fs::read_to_string(dir.join(cur_file))
            .ok()
            .and_then(|t| t.trim().parse().ok())?;
        let inactive = std::fs::read_to_string(dir.join("memory.stat"))
            .ok()
            .and_then(|t| inactive_keys.iter().find_map(|k| parse_stat_field(&t, k)))
            .unwrap_or(0);
        return Some(CgroupMem {
            limit,
            used: current.saturating_sub(inactive),
        });
    }
    None
}

/// Reads real `/proc` and cgroup files.
///
/// Cache: the resolved cgroup directory list (at most one entry per process lifetime,
/// owned by this sampler; never invalidated because a process does not change cgroup
/// in practice). The limit and usage files themselves are re-read on every sample.
pub struct LinuxSampler {
    page_size: u64,
    cgroup: OnceLock<Option<CgroupDirs>>,
}

impl Default for LinuxSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxSampler {
    pub fn new() -> Self {
        // SAFETY: sysconf has no preconditions.
        let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Self {
            page_size: if ps > 0 { ps as u64 } else { 4096 },
            cgroup: OnceLock::new(),
        }
    }

    fn rss_of(&self, pid: u32) -> Option<u64> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
        parse_statm_rss(&text, self.page_size)
    }

    /// Children of `pid` over all its threads; `None` when the kernel exposes no
    /// `children` file.
    fn children_of(pid: u32) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
        for t in tasks.flatten() {
            let path = t.path().join("children");
            let text = std::fs::read_to_string(path).ok()?;
            out.extend(
                text.split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok()),
            );
        }
        Some(out)
    }

    fn tree_pids_bfs(root: u32) -> Option<Vec<u32>> {
        let mut seen: HashSet<u32> = HashSet::from([root]);
        let mut order = vec![root];
        let mut i = 0;
        while i < order.len() && order.len() < MAX_TREE_PIDS {
            let kids = Self::children_of(order[i]);
            i += 1;
            match kids {
                Some(kids) => {
                    for k in kids {
                        if seen.insert(k) {
                            order.push(k);
                        }
                    }
                }
                // The root itself has no children file: unsupported kernel.
                None if i == 1 => return None,
                // A descendant vanished mid-walk.
                None => {}
            }
        }
        Some(order)
    }

    /// One scan of `/proc/*/stat` shared by every root.
    fn scan_stats() -> HashMap<u32, ProcStat> {
        let mut map = HashMap::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return map;
        };
        for e in dir.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                && let Some(st) = parse_proc_stat(&text)
            {
                map.insert(pid, st);
            }
        }
        map
    }

    fn tree_pids_scan(root: u32, stats: &HashMap<u32, ProcStat>) -> Vec<u32> {
        let mut members: HashSet<u32> = stats
            .iter()
            .filter(|(_, s)| s.pgrp == root)
            .map(|(p, _)| *p)
            .collect();
        if stats.contains_key(&root) {
            members.insert(root);
        }
        // Add descendants through the ppid chain until no growth (depth-bounded).
        for _ in 0..64 {
            let before = members.len();
            for (pid, st) in stats {
                if members.len() < MAX_TREE_PIDS && members.contains(&st.ppid) {
                    members.insert(*pid);
                }
            }
            if members.len() == before {
                break;
            }
        }
        members.into_iter().collect()
    }
}

impl ResourceSampler for LinuxSampler {
    fn supported(&self) -> bool {
        Path::new("/proc/meminfo").exists()
    }

    fn host(&self, spool_dir: &Path) -> Option<HostSample> {
        let mem = parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)?;
        let cg = self
            .cgroup
            .get_or_init(resolve_cgroup_dirs)
            .as_ref()
            .and_then(read_cgroup_mem);
        let (total, available) = effective_memory(&mem, cg);
        Some(HostSample {
            total,
            available,
            swap_used: mem.swap_total.saturating_sub(mem.swap_free),
            disk_free: disk_free_bytes(spool_dir),
        })
    }

    fn trees(&self, roots: &[TreeRoot]) -> Vec<TreeSample> {
        let mut scanned: Option<HashMap<u32, ProcStat>> = None;
        let mut out = Vec::with_capacity(roots.len());
        for r in roots {
            let pids = match Self::tree_pids_bfs(r.pgid) {
                Some(p) => p,
                None => {
                    let stats = scanned.get_or_insert_with(Self::scan_stats);
                    Self::tree_pids_scan(r.pgid, stats)
                }
            };
            let mut rss = 0u64;
            let mut any = false;
            for pid in pids {
                if let Some(b) = self.rss_of(pid) {
                    rss = rss.saturating_add(b);
                    any = true;
                }
            }
            if any {
                out.push(TreeSample {
                    id: r.id.clone(),
                    rss,
                    started: r.started,
                    protected: r.protected,
                });
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod linux_tests;
