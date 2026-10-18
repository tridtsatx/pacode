//! Per-turn firing limits.
//!
//! Owner: one `TurnLimiter` per agent, held by the caller (pacode-core keeps it in
//! its per-session guard registry). Size: at most one counter per rule name, so it
//! is bounded by the rule count. Invalidation: [`TurnLimiter::reset`] at the first
//! model step of every user turn. Nothing here runs on a timer.

use std::collections::{BTreeMap, BTreeSet};

use crate::scan::Finding;

/// Streams aborted (and retried) per turn, over all rules.
pub const MAX_STREAM_RETRIES_PER_TURN: u32 = 2;

#[derive(Debug, Default)]
pub struct TurnLimiter {
    fired: BTreeMap<String, u32>,
    retries: u32,
}

impl TurnLimiter {
    pub fn reset(&mut self) {
        self.fired.clear();
        self.retries = 0;
    }

    pub fn retries(&self) -> u32 {
        self.retries
    }

    pub fn fired(&self, rule: &str) -> u32 {
        self.fired.get(rule).copied().unwrap_or(0)
    }

    fn has_budget(&self, f: &Finding) -> bool {
        self.fired(&f.rule) < f.max_per_turn
    }

    fn consume(&mut self, kept: &[Finding]) {
        let distinct: BTreeSet<&str> = kept.iter().map(|f| f.rule.as_str()).collect();
        for rule in distinct {
            *self.fired.entry(rule.to_string()).or_insert(0) += 1;
        }
    }

    /// After-write report: keep findings of rules that still have budget this turn;
    /// every distinct kept rule consumes one firing.
    pub fn admit(&mut self, findings: Vec<Finding>) -> Vec<Finding> {
        let kept: Vec<Finding> = findings
            .into_iter()
            .filter(|f| self.has_budget(f))
            .collect();
        self.consume(&kept);
        kept
    }

    /// Stream hit: like [`admit`](Self::admit) but also needs one of the
    /// [`MAX_STREAM_RETRIES_PER_TURN`] retries; returns nothing (and consumes
    /// nothing) once they are spent, so the after-write scan still catches it.
    pub fn admit_stream(&mut self, findings: Vec<Finding>) -> Vec<Finding> {
        if self.retries >= MAX_STREAM_RETRIES_PER_TURN {
            return Vec::new();
        }
        let kept: Vec<Finding> = findings
            .into_iter()
            .filter(|f| self.has_budget(f))
            .collect();
        if kept.is_empty() {
            return kept;
        }
        self.retries += 1;
        self.consume(&kept);
        kept
    }
}
