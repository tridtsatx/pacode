//! The memory policy: what a tool process is allowed to use, and the arithmetic
//! that turns `[ui] tool_memory_percent` into a byte count.
//!
//! # Why two ceilings and not one
//!
//! A single process is capped at `percent` of machine RAM. That is only a
//! safety limit if the *machine* is what gets protected, so with
//! `tool_memory_split_sessions = true` the same number is divided between the
//! sessions that are live right now: one session gets 80%, two get 40% each,
//! four get 20% each. The sum of what a set of concurrently running sessions
//! may allocate stays at the ceiling. With the split off, each session gets the
//! full ceiling and the sum is `n × ceiling` — the machine can still be eaten,
//! which is the tradeoff the setting names.
//!
//! # What the numbers mean
//!
//! - `percent == 0` disables the cap entirely: no limit is computed, no
//!   environment is injected, and the tool is told so.
//! - A ceiling is rounded **down** to a whole page. Rounding up would let a
//!   process exceed the ceiling by up to a page, which is exactly the kind of
//!   slack a ceiling exists to remove.
//! - The machine RAM used here is total RAM, not `MemAvailable`. The cap is
//!   about what the machine can physically hold, and a cap that shrinks every
//!   time something else allocates would be a throttle, not a ceiling.
//!
//! The enforcement mechanism is not decided here: [`crate::memlimit`] picks a
//! backend and [`LimitKind`] records what the tool is actually being held to.

use std::fmt;

use pacode_types::UiConfig;
use pacode_types::config::TOOL_MEMORY_MIN_PERCENT;

/// Kernel page size used for rounding and for cgroup byte maths. cgroup v2
/// expresses `memory.max` in bytes and rounds internally to pages, so anything
/// the cgroup stores is a page multiple; rounding here makes the number the
/// tool is told equal the number the kernel enforces.
pub const PAGE_BYTES: u64 = 4096;

/// What the limit is measured against. The two are not interchangeable and the
/// tool has to know which one it is living under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// cgroup v2 `memory.max` on a per-session leaf. Counts *resident* pages,
    /// so a JVM that reserves a large address space but touches little of it
    /// is not punished for the reservation.
    Cgroup,
    /// `RLIMIT_AS`. Counts *virtual* address space, so it is strictly harsher
    /// than the memory a process actually uses. See [`crate::memrlimit`] for
    /// what that costs and what the product does about it.
    AddressSpace,
}

impl LimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cgroup => "cgroup-v2",
            Self::AddressSpace => "address-space",
        }
    }
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The inputs to the ceiling, resolved once: what the user asked for, whether
/// the ceiling is shared, and how big the machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryPolicy {
    /// `ui.tool_memory_percent`; `0` disables the cap.
    pub percent: u8,
    /// `ui.tool_memory_split_sessions`.
    pub split_sessions: bool,
    /// Total machine RAM. `0` means "could not be determined", which disables
    /// the cap: a percentage of an unknown machine is not a number.
    pub machine_ram_bytes: u64,
}

impl MemoryPolicy {
    /// Build from the user's config plus a probed RAM size. The size is a
    /// parameter rather than a probe inside this function so the arithmetic is
    /// testable without a machine.
    pub fn from_ui(ui: &UiConfig, machine_ram_bytes: u64) -> Self {
        Self {
            percent: ui.tool_memory_percent,
            split_sessions: ui.tool_memory_split_sessions,
            machine_ram_bytes,
        }
    }

    /// A cap that does nothing, for callers that have no config yet.
    pub fn disabled() -> Self {
        Self {
            percent: 0,
            split_sessions: true,
            machine_ram_bytes: 0,
        }
    }

    /// Whether any byte count can be computed at all.
    pub fn is_enabled(&self) -> bool {
        self.percent > 0 && self.machine_ram_bytes > 0
    }
    /// Ceiling for one process while `live_sessions` sessions are live.
    ///
    /// `None` means "no cap".
    pub fn ceiling_for(&self, live_sessions: usize) -> Option<u64> {
        if !self.is_enabled() {
            return None;
        }
        let total = self
            .machine_ram_bytes
            .saturating_mul(u64::from(self.percent))
            / 100;
        if !self.split_sessions {
            return Some(round_down_to_page(total).max(PAGE_BYTES));
        }
        // Zero live sessions is the moment before the first one opens: the
        // session about to start gets the full ceiling, not a share of nothing.
        let share = total / live_sessions.max(1) as u64;
        Some(round_down_to_page(share.max(self.division_floor())).max(PAGE_BYTES))
    }

    /// The share a session may not be divided below, in bytes. Division
    /// continues to shrink until it reaches `TOOL_MEMORY_MIN_PERCENT` of the
    /// machine, the same floor the settings registry uses, because below that
    /// a "ceiling" stops being a safety limit and starts being an outage: a
    /// daemon with a hundred idle sessions open would otherwise hand every new
    /// process 0.8% of RAM and nothing would run at all. The sessions above the
    /// floor take proportionally less, so the sum can exceed the ceiling once
    /// there are more than `percent / TOOL_MEMORY_MIN_PERCENT` of them — the
    /// machine, not the arithmetic, is then the limit, which is what the hard
    /// limit and the OOM killer are for.
    pub fn division_floor(&self) -> u64 {
        round_down_to_page(
            self.machine_ram_bytes
                .saturating_mul(u64::from(TOOL_MEMORY_MIN_PERCENT))
                / 100,
        )
    }

    /// The ceiling a soft limit may be raised to: the machine's physical RAM.
    ///
    /// This is the hard half of the soft/hard split. The soft limit is the
    /// session's share from [`Self::ceiling_for`]; a granted raise moves it
    /// anywhere up to here on a process that is already running, with no
    /// re-exec and no privilege — which is the whole reason the hard limit is
    /// the physical size rather than a second policy number. `None` when the
    /// cap is off: with no soft limit there is nothing to raise.
    pub fn hard_max_bytes(&self) -> Option<u64> {
        if !self.is_enabled() {
            return None;
        }
        Some(self.machine_ram_bytes)
    }
}

/// The resolved answer for one process: the number, the ceiling it cannot be
/// raised past, what is enforcing it, and what the ceiling was divided by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCeiling {
    pub bytes: u64,
    pub hard_max_bytes: u64,
    pub kind: LimitKind,
    pub live_sessions: usize,
    pub percent: u8,
    pub split_sessions: bool,
    pub machine_ram_bytes: u64,
}

impl MemoryCeiling {
    /// One line a tool can print and a model can read: how much it may use,
    /// what it was divided from, and what is doing the counting.
    pub fn describe(&self) -> String {
        let shared = if self.split_sessions && self.live_sessions > 1 {
            format!("divided between {} live sessions", self.live_sessions)
        } else {
            "not divided".to_string()
        };
        format!(
            "{} of {} machine RAM ({}% ceiling, {}, enforced by {}); \
             a process may ask the user to raise this to at most {}",
            human_bytes(self.bytes),
            human_bytes(self.machine_ram_bytes),
            self.percent,
            shared,
            self.kind,
            human_bytes(self.hard_max_bytes),
        )
    }

    /// Environment for the tool. Names are stable — a script, a hook, and the
    /// model itself can all read them.
    pub fn environment(&self) -> Vec<(String, String)> {
        vec![
            (
                "PACODE_MEMORY_LIMIT_BYTES".to_string(),
                self.bytes.to_string(),
            ),
            (
                "PACODE_MEMORY_HARD_MAX_BYTES".to_string(),
                self.hard_max_bytes.to_string(),
            ),
            (
                "PACODE_MEMORY_LIMIT_KIND".to_string(),
                self.kind.as_str().to_string(),
            ),
            ("PACODE_MEMORY_LIMIT".to_string(), self.describe()),
        ]
    }
}

impl fmt::Display for MemoryCeiling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Human-readable size, binary units, one decimal.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn round_down_to_page(bytes: u64) -> u64 {
    bytes / PAGE_BYTES * PAGE_BYTES
}

#[cfg(test)]
#[path = "memcap_tests.rs"]
mod memcap_tests;
