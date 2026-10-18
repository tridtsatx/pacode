//! Pure resource policy: a state machine fed with samples and a clock, answering with
//! decisions. No I/O, no timers, no threads, so every rule is unit-testable with a fake
//! clock (`Instant` plus a `Duration`).
//!
//! Rules (all "soft"):
//! - reserve = `min_free_percent` of the effective total; `BREACH_LEVEL = reserve -
//!   overshoot`.
//! - `available >= reserve`: Ok. Between `BREACH_LEVEL` and the reserve: Warn (new
//!   spawns are refused, nothing is killed). Below `BREACH_LEVEL`: Breach.
//! - A Breach only turns into a kill after it has lasted `grace` and memory is still
//!   under the reserve; leaving Breach needs `available >= reserve` for [`CLEAR_HOLD`].
//! - The victim is the largest unprotected tree older than [`YOUNG_TREE`]; it gets
//!   SIGTERM, and SIGKILL only after `term_wait`. After any kill the policy waits one
//!   full `grace` before choosing another victim.
//! - `max_process_percent` caps one tree with the same overshoot/grace rules.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use pacode_types::{ResourcesConfig, TaskId};

use super::sampler::{HostSample, TreeSample};

/// A tree younger than this is never a victim (it gets a fresh grace).
pub const YOUNG_TREE: Duration = Duration::from_secs(2);
/// Time above the reserve required to leave Breach (anti-flap).
pub const CLEAR_HOLD: Duration = Duration::from_secs(2);
/// Minimum spacing of repeated warning notices while the level does not change.
pub const WARN_REEMIT: Duration = Duration::from_secs(30);

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyParams {
    pub enabled: bool,
    pub min_free_percent: u64,
    pub overshoot: u64,
    pub grace: Duration,
    pub term_wait: Duration,
    pub max_process_percent: u64,
    pub min_free_disk: u64,
    pub idle_interval: Duration,
    pub pressure_interval: Duration,
}

impl From<&ResourcesConfig> for PolicyParams {
    fn from(c: &ResourcesConfig) -> Self {
        Self {
            enabled: c.enabled,
            min_free_percent: u64::from(c.min_free_percent.min(95)),
            overshoot: c.overshoot_mb.saturating_mul(MB),
            grace: Duration::from_secs(c.grace_secs),
            term_wait: Duration::from_secs(c.term_wait_secs),
            max_process_percent: u64::from(c.max_process_percent.min(100)),
            min_free_disk: c.min_free_disk_mb.saturating_mul(MB),
            idle_interval: Duration::from_millis(c.sample_ms_idle_headroom.max(1)),
            pressure_interval: Duration::from_millis(c.sample_ms_pressure.max(1)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Breach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillSignal {
    Term,
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefuseKind {
    Memory,
    Disk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Entered (or still in, rate-limited) Warn/Breach.
    Warn {
        level: Level,
        available: u64,
        total: u64,
    },
    /// New spawns must be refused with `message` (emitted on every step while true).
    Refuse { kind: RefuseKind, message: String },
    /// Signal the whole process group of `tree`; `reason` is the text the task reports.
    Kill {
        tree: TaskId,
        signal: KillSignal,
        reason: String,
    },
    /// Back to Ok.
    Clear,
}

pub fn fmt_bytes(b: u64) -> String {
    if b >= GB {
        format!("{:.1} GB", b as f64 / GB as f64)
    } else {
        format!("{} MB", b / MB)
    }
}

pub fn free_percent(host: &HostSample) -> u64 {
    if host.total == 0 {
        return 100;
    }
    host.available.saturating_mul(100) / host.total
}

pub fn reserve_bytes(p: &PolicyParams, total: u64) -> u64 {
    total / 100 * p.min_free_percent + (total % 100) * p.min_free_percent / 100
}

/// The text of a refused spawn for low memory.
pub fn memory_message(p: &PolicyParams, host: &HostSample) -> String {
    format!(
        "low resources: {} free of {} ({}%), reserve is {}%; wait for running commands to finish or stop one (see /bg)",
        fmt_bytes(host.available),
        fmt_bytes(host.total),
        free_percent(host),
        p.min_free_percent
    )
}

pub fn disk_message(p: &PolicyParams, free: u64) -> String {
    format!(
        "low resources: {} free on the output spool disk, minimum is {}; free some disk space or clear old task output",
        fmt_bytes(free),
        fmt_bytes(p.min_free_disk)
    )
}

/// Refusal for a new process or subagent given a fresh host sample, if any. Used by
/// `admit`; independent of the running state machine so a stale state can never refuse.
pub fn refusal(p: &PolicyParams, host: &HostSample) -> Option<String> {
    if !p.enabled || host.total == 0 {
        return None;
    }
    if host.available < reserve_bytes(p, host.total) {
        return Some(memory_message(p, host));
    }
    match host.disk_free {
        Some(d) if p.min_free_disk > 0 && d < p.min_free_disk => Some(disk_message(p, d)),
        _ => None,
    }
}

fn kill_reason(p: &PolicyParams, host: &HostSample, tree_rss: u64) -> String {
    format!(
        "killed by pacode resource guard: process tree used {}; host had {} ({}%) free, reserve is {}%. Reduce memory use (stream/chunk the data, lower parallelism) and retry.",
        fmt_bytes(tree_rss),
        fmt_bytes(host.available),
        free_percent(host),
        p.min_free_percent
    )
}

fn cap_reason(p: &PolicyParams, tree_rss: u64, limit: u64) -> String {
    format!(
        "killed by pacode resource guard: process tree used {}, above the per-process limit of {} ({}% of memory). Reduce memory use (stream/chunk the data, lower parallelism) and retry.",
        fmt_bytes(tree_rss),
        fmt_bytes(limit),
        p.max_process_percent
    )
}

#[derive(Clone, Debug)]
enum Stop {
    Term { deadline: Instant, reason: String },
    Killed,
}

/// The state machine. Owned by the sampling task of the guard (one instance per
/// sampling run); holds one entry per supervised tree at most and forgets trees that
/// disappear from the samples.
pub struct Policy {
    p: PolicyParams,
    level: Level,
    breach_since: Option<Instant>,
    clear_since: Option<Instant>,
    last_kill: Option<Instant>,
    last_notice: Option<Instant>,
    over_since: HashMap<TaskId, Instant>,
    stopping: HashMap<TaskId, Stop>,
}

impl Policy {
    pub fn new(p: PolicyParams) -> Self {
        Self {
            p,
            level: Level::Ok,
            breach_since: None,
            clear_since: None,
            last_kill: None,
            last_notice: None,
            over_since: HashMap::new(),
            stopping: HashMap::new(),
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// Sampling interval for the next tick: relaxed while comfortably above the reserve.
    pub fn sample_interval(&self, host: &HostSample) -> Duration {
        let reserve = reserve_bytes(&self.p, host.total);
        let headroom = reserve.saturating_add(self.p.overshoot.saturating_mul(2));
        if self.level == Level::Ok && host.available > headroom && self.stopping.is_empty() {
            self.p.idle_interval
        } else {
            self.p.pressure_interval
        }
    }

    pub fn step(&mut self, now: Instant, host: HostSample, trees: &[TreeSample]) -> Vec<Decision> {
        let mut out = Vec::new();
        if !self.p.enabled || host.total == 0 {
            return out;
        }
        self.stopping
            .retain(|id, _| trees.iter().any(|t| &t.id == id));
        self.over_since
            .retain(|id, _| trees.iter().any(|t| &t.id == id));

        let reserve = reserve_bytes(&self.p, host.total);
        let breach_level = reserve.saturating_sub(self.p.overshoot);
        let prev = self.level;
        self.update_level(now, host.available, reserve, breach_level);

        match (prev, self.level) {
            (_, Level::Ok) if prev != Level::Ok => {
                self.last_notice = None;
                out.push(Decision::Clear);
            }
            (_, Level::Ok) => {}
            (_, level) => {
                let stale = self
                    .last_notice
                    .is_none_or(|t| now.saturating_duration_since(t) >= WARN_REEMIT);
                if level != prev || stale {
                    self.last_notice = Some(now);
                    out.push(Decision::Warn {
                        level,
                        available: host.available,
                        total: host.total,
                    });
                }
                out.push(Decision::Refuse {
                    kind: RefuseKind::Memory,
                    message: memory_message(&self.p, &host),
                });
            }
        }
        if let Some(d) = host.disk_free
            && self.p.min_free_disk > 0
            && d < self.p.min_free_disk
        {
            out.push(Decision::Refuse {
                kind: RefuseKind::Disk,
                message: disk_message(&self.p, d),
            });
        }

        self.escalate(now, &mut out);
        self.enforce_process_cap(now, &host, trees, &mut out);
        self.enforce_breach(now, &host, reserve, trees, &mut out);
        out
    }

    fn update_level(&mut self, now: Instant, available: u64, reserve: u64, breach_level: u64) {
        match self.level {
            Level::Ok | Level::Warn => {
                if available < breach_level {
                    self.level = Level::Breach;
                    self.breach_since = Some(now);
                    self.clear_since = None;
                } else if available < reserve {
                    self.level = Level::Warn;
                } else {
                    self.level = Level::Ok;
                }
            }
            Level::Breach => {
                if available >= reserve {
                    let since = *self.clear_since.get_or_insert(now);
                    if now.saturating_duration_since(since) >= CLEAR_HOLD {
                        self.level = Level::Ok;
                        self.breach_since = None;
                        self.clear_since = None;
                    }
                } else {
                    self.clear_since = None;
                }
            }
        }
    }

    /// SIGTERM was sent and `term_wait` has elapsed with the tree still alive: SIGKILL.
    fn escalate(&mut self, now: Instant, out: &mut Vec<Decision>) {
        let due: Vec<(TaskId, String)> = self
            .stopping
            .iter()
            .filter_map(|(id, st)| match st {
                Stop::Term { deadline, reason } if now >= *deadline => {
                    Some((id.clone(), reason.clone()))
                }
                Stop::Term { .. } | Stop::Killed => None,
            })
            .collect();
        for (id, reason) in due {
            self.stopping.insert(id.clone(), Stop::Killed);
            out.push(Decision::Kill {
                tree: id,
                signal: KillSignal::Kill,
                reason,
            });
        }
    }

    fn start_stop(&mut self, now: Instant, id: &TaskId, reason: String, out: &mut Vec<Decision>) {
        self.stopping.insert(
            id.clone(),
            Stop::Term {
                deadline: now + self.p.term_wait,
                reason: reason.clone(),
            },
        );
        self.last_kill = Some(now);
        out.push(Decision::Kill {
            tree: id.clone(),
            signal: KillSignal::Term,
            reason,
        });
    }

    fn eligible(&self, now: Instant, t: &TreeSample) -> bool {
        !t.protected
            && now.saturating_duration_since(t.started) >= YOUNG_TREE
            && !self.stopping.contains_key(&t.id)
    }

    fn enforce_process_cap(
        &mut self,
        now: Instant,
        host: &HostSample,
        trees: &[TreeSample],
        out: &mut Vec<Decision>,
    ) {
        if self.p.max_process_percent == 0 {
            return;
        }
        let cap = host.total / 100 * self.p.max_process_percent;
        let limit = cap.saturating_add(self.p.overshoot);
        for t in trees {
            if t.rss <= limit {
                self.over_since.remove(&t.id);
                continue;
            }
            let since = *self.over_since.entry(t.id.clone()).or_insert(now);
            if now.saturating_duration_since(since) >= self.p.grace && self.eligible(now, t) {
                let reason = cap_reason(&self.p, t.rss, cap);
                self.start_stop(now, &t.id, reason, out);
            }
        }
    }

    fn enforce_breach(
        &mut self,
        now: Instant,
        host: &HostSample,
        reserve: u64,
        trees: &[TreeSample],
        out: &mut Vec<Decision>,
    ) {
        let Some(since) = self.breach_since else {
            return;
        };
        if self.level != Level::Breach
            || host.available >= reserve
            || now.saturating_duration_since(since) < self.p.grace
        {
            return;
        }
        let cooled = self
            .last_kill
            .is_none_or(|t| now.saturating_duration_since(t) >= self.p.grace);
        if !cooled {
            return;
        }
        let victim = trees
            .iter()
            .filter(|t| self.eligible(now, t))
            .max_by_key(|t| t.rss)
            .map(|t| (t.id.clone(), t.rss));
        if let Some((id, rss)) = victim {
            let reason = kill_reason(&self.p, host, rss);
            self.start_stop(now, &id, reason, out);
        }
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod policy_tests;
