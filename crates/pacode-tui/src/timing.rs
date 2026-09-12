//! Startup and frame timing instrumentation.
//!
//! Gated behind `PACODE_TIMING=1` (spec §8.3); `PACODE_DEBUG_TIMING=1` is kept
//! as an alias. Each entry point (`cli::main`, `app::run`) reads the env var
//! once into a bool, so the steady-state cost is a few `Instant::now()` calls
//! and one branch per mark. Output goes to stderr, never into the TUI frame.

use std::time::{Duration, Instant};

/// Whether timing output is enabled. Read once at startup.
pub fn enabled() -> bool {
    fn is_on(name: &str) -> bool {
        std::env::var(name).is_ok_and(|v| v == "1")
    }
    is_on("PACODE_TIMING") || is_on("PACODE_DEBUG_TIMING")
}

/// Print `[timing] <label>: <ms>` (elapsed since `start`) to stderr when `on`.
pub fn mark(on: bool, label: &str, start: Instant) {
    if on {
        eprintln!(
            "[timing] {label}: {:.2} ms",
            start.elapsed().as_secs_f64() * 1e3
        );
    }
}

/// The frame budget for a detected refresh rate (§8.3): 8 ms at 120 Hz,
/// 16 ms at 60 Hz — half the frame interval, interpolated between.
/// `None` (unknown Hz) budgets for the 60 Hz floor.
pub fn frame_budget(hz: Option<u16>) -> Duration {
    match hz.unwrap_or(60) {
        h if h >= 120 => Duration::from_millis(8),
        _ => Duration::from_millis(16),
    }
}

/// Warn on a frame that ran over `budget` — the §8.3 diagnostic for
/// render-path regressions. Only meaningful when `on` (`PACODE_TIMING=1`).
pub fn warn_frame_over_budget(on: bool, elapsed: Duration, budget: Duration) {
    if on && elapsed > budget {
        eprintln!(
            "[timing] frame over budget: {:.2} ms (budget {:.0} ms)",
            elapsed.as_secs_f64() * 1e3,
            budget.as_secs_f64() * 1e3,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_budget_matches_spec() {
        assert_eq!(frame_budget(Some(120)), Duration::from_millis(8));
        assert_eq!(frame_budget(Some(144)), Duration::from_millis(8));
        assert_eq!(frame_budget(Some(60)), Duration::from_millis(16));
        assert_eq!(frame_budget(Some(30)), Duration::from_millis(16));
        assert_eq!(frame_budget(None), Duration::from_millis(16));
    }

    #[test]
    fn frame_warn_only_over_budget_and_enabled() {
        // No output asserts — the function must simply not panic in either
        // branch; stderr writes are observable side effects only.
        warn_frame_over_budget(false, Duration::from_secs(1), Duration::from_millis(1));
        warn_frame_over_budget(true, Duration::from_millis(1), Duration::from_millis(16));
        warn_frame_over_budget(true, Duration::from_millis(17), Duration::from_millis(16));
    }
}
