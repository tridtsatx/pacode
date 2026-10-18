//! Non-Linux samplers. macOS is best effort: `sysctl hw.memsize` + `vm_stat` for the
//! host and `ps -o rss= -g <pgid>` for a tree. The external commands run only from a
//! sampling tick, i.e. only while a supervised process is alive. Other platforms get
//! [`UnsupportedSampler`]: the guard disables itself and says so once.

use std::path::Path;
use std::process::Command;

use super::sampler::{HostSample, ResourceSampler, TreeRoot, TreeSample, disk_free_bytes};

/// Pages that can be reclaimed without swapping, from `vm_stat` output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmStat {
    pub page_size: u64,
    pub free: u64,
    pub inactive: u64,
    pub speculative: u64,
    pub purgeable: u64,
}

impl VmStat {
    pub fn available_bytes(&self) -> u64 {
        self.free
            .saturating_add(self.inactive)
            .saturating_add(self.speculative)
            .saturating_add(self.purgeable)
            .saturating_mul(self.page_size)
    }
}

/// Parse `vm_stat`: header `... (page size of 16384 bytes)` then `Pages free:   123.`
pub fn parse_vm_stat(text: &str) -> Option<VmStat> {
    let header = text.lines().next()?;
    let page_size: u64 = header
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let field = |name: &str| -> u64 {
        text.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == name)
                    .then(|| v.trim().trim_end_matches('.').parse::<u64>().ok())
                    .flatten()
            })
            .unwrap_or(0)
    };
    Some(VmStat {
        page_size,
        free: field("Pages free"),
        inactive: field("Pages inactive"),
        speculative: field("Pages speculative"),
        purgeable: field("Pages purgeable"),
    })
}

/// Sum of `ps -o rss=` output (one kB value per line) in bytes.
pub fn parse_ps_rss_bytes(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .fold(0u64, |acc, kb| acc.saturating_add(kb.saturating_mul(1024)))
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// macOS sampler (unverified on real hardware: written from the documented tool output).
#[derive(Default)]
#[allow(dead_code)]
pub struct MacSampler;

impl ResourceSampler for MacSampler {
    fn supported(&self) -> bool {
        true
    }

    fn host(&self, spool_dir: &Path) -> Option<HostSample> {
        let total: u64 = run("sysctl", &["-n", "hw.memsize"])?.trim().parse().ok()?;
        let vm = parse_vm_stat(&run("vm_stat", &[])?)?;
        Some(HostSample {
            total,
            available: vm.available_bytes().min(total),
            swap_used: 0,
            disk_free: disk_free_bytes(spool_dir),
        })
    }

    fn trees(&self, roots: &[TreeRoot]) -> Vec<TreeSample> {
        roots
            .iter()
            .filter_map(|r| {
                let text = run("ps", &["-o", "rss=", "-g", &r.pgid.to_string()])?;
                let rss = parse_ps_rss_bytes(&text);
                (rss > 0).then(|| TreeSample {
                    id: r.id.clone(),
                    rss,
                    started: r.started,
                    protected: r.protected,
                })
            })
            .collect()
    }
}

/// Platforms where nothing can be measured.
#[derive(Default)]
#[allow(dead_code)]
pub struct UnsupportedSampler;

impl ResourceSampler for UnsupportedSampler {
    fn supported(&self) -> bool {
        false
    }

    fn host(&self, _spool_dir: &Path) -> Option<HostSample> {
        None
    }

    fn trees(&self, _roots: &[TreeRoot]) -> Vec<TreeSample> {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod fallback_tests;
