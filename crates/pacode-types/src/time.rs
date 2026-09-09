//! Wall-clock helpers shared by all crates (std only).

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch. Never panics: a clock before 1970 yields 0.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Microseconds since the Unix epoch (used for id generation).
pub(crate) fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Format a duration: `342ms` under a second, `7.3s` under 10s, `42s` under a minute,
/// `1m05s` under an hour, `1h02m` above.
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        let val = ms as f64 / 1000.0;
        format!("{val:.1}s")
    } else if ms < 60_000 {
        let secs = ms / 1000;
        format!("{secs}s")
    } else if ms < 3_600_000 {
        let secs = ms / 1000;
        let mins = secs / 60;
        let rem_secs = secs % 60;
        format!("{mins}m{rem_secs:02}s")
    } else {
        let secs = ms / 1000;
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{hours}h{mins:02}m")
    }
}

/// Format a token count as `9.1k` / `207.6k` / `1.2M` / `842`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let val = n as f64 / 1_000_000.0;
        format!("{val:.1}M")
    } else if n >= 1_000 {
        let val = n as f64 / 1_000.0;
        format!("{val:.1}k")
    } else {
        n.to_string()
    }
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod time_tests;
