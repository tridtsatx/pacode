//! Turn statistics line formatting (spec §4).
//!
//! Replaces token counters with pacman-themed verbs and local completion time:
//! `{Verb} for {dur} · done {h:mm AM/PM}`.

use chrono::{DateTime, Local};
use pacode_types::time::format_duration_ms;
use rand::Rng;

#[cfg(test)]
#[path = "stats_tests.rs"]
mod stats_tests;

pub const PACMAN_VERBS: &[&str] = &[
    "Munched",
    "Chomped",
    "Nibbled",
    "Crunched",
    "Gobbled",
    "Devoured",
    "Snacked",
    "Feasted",
    "Dotted",
    "Waka-waka'd",
    "Chewed",
    "Gnawed",
];

/// Pick a random pacman-themed verb.
pub fn pick_verb() -> &'static str {
    let mut rng = rand::rng();
    let idx = rng.random_range(0..PACMAN_VERBS.len());
    PACMAN_VERBS[idx]
}

/// Format the turn stats line deterministically.
pub fn stats_line_with(verb: &str, duration_ms: u64, now: DateTime<Local>) -> String {
    let dur = format_duration_ms(duration_ms);
    let done = now.format("%-I:%M %p");
    format!("{verb} for {dur} · done {done}")
}

/// Format the turn stats line with a randomly selected verb and current local time.
pub fn stats_line(duration_ms: u64) -> String {
    stats_line_with(pick_verb(), duration_ms, Local::now())
}
