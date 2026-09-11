//! Startup timing instrumentation.
//!
//! Gated behind `PACODE_DEBUG_TIMING=1`. Each entry point (`cli::main`,
//! `app::run`) reads the env var once into a bool, so the steady-state cost is
//! a few `Instant::now()` calls and one branch per mark. Output goes to
//! stderr, never into the TUI frame.

use std::time::Instant;

/// Whether timing output is enabled. Read once at startup.
pub fn enabled() -> bool {
    std::env::var("PACODE_DEBUG_TIMING").is_ok_and(|v| v == "1")
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
