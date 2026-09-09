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

/// Format a duration: `840ms` under a second, `12.3s` under a minute, `1m47s` under an
/// hour, `1h02m` above.
pub fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Format a token count as `9.1k` / `207.6k` / `1.2M` / `842`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(400), "400ms");
        assert_eq!(format_duration_ms(41_000), "41.0s");
        assert_eq!(format_duration_ms(107_000), "1m47s");
        assert_eq!(format_duration_ms(3_720_000), "1h02m");
    }

    #[test]
    fn tokens() {
        assert_eq!(format_tokens(842), "842");
        assert_eq!(format_tokens(9_100), "9.1k");
        assert_eq!(format_tokens(207_600), "207.6k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }
}
