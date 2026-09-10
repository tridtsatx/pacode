//! Phrase of the day shown on the right of the header (spec §9 extension).
//!
//! The list is static and lives in the binary; the phrase is picked from the day
//! index so it is stable for a whole day and needs no state, timer or storage.

#[cfg(test)]
#[path = "phrases_tests.rs"]
mod phrases_tests;

/// Static phrase pool. Keep entries short: they share the header row with the title.
const PHRASES: &[&str] = &[
    "eat the dots, dodge the ghosts",
    "waka waka, ship it",
    "one more turn and it compiles",
    "the ghosts are just tests you skipped",
    "power pellet: cargo clippy",
    "no unwrap survives contact with prod",
    "a warning today is a panic tomorrow",
    "small diffs, big sleep",
    "measure first, then optimise",
    "the maze always has an exit",
    "borrow, don't clone",
    "idle means idle: no timers",
    "read the error, it is telling you",
    "fast is a feature",
    "delete more than you add",
    "every cache needs an owner",
    "green tests, quiet mind",
    "the bug is in the last place you looked",
    "name it well and it explains itself",
    "ship the boring version first",
    "context is expensive, spend it wisely",
    "a hack today is ten patches tomorrow",
    "the terminal is the best UI",
    "keep the maze tidy",
];

/// Phrase for `day` (days since the Unix epoch). Deterministic and total.
pub fn phrase_of_the_day(day: u64) -> &'static str {
    let idx = (day % PHRASES.len() as u64) as usize;
    PHRASES[idx]
}

/// Days since the Unix epoch for `now_ms`.
pub fn day_index(now_ms: u64) -> u64 {
    now_ms / 86_400_000
}
