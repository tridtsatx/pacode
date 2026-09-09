//! Tiny file logger for the `log` facade. No timers, no background thread: each
//! record is formatted and appended under a mutex. Level from `CODEAPP_LOG`
//! (`error|warn|info|debug|trace`, default `info`).

use std::path::Path;

/// Install the global logger writing to `path`. Calling twice is a no-op.
/// Format: `2026-09-09T12:34:56.789Z INFO  target: message`.
pub fn init_file_logger(path: &Path, level: log::LevelFilter) -> std::io::Result<()> {
    let _ = (path, level);
    todo!("logging::init_file_logger")
}

/// Parse `CODEAPP_LOG`; `None` when unset or invalid.
pub fn level_from_env(value: Option<&str>) -> Option<log::LevelFilter> {
    let _ = value;
    todo!("logging::level_from_env")
}
