//! Tiny file logger for the `log` facade. No timers, no background thread: each
//! record is formatted and appended under a mutex. Level from `PACODE_LOG`
//! (`error|warn|info|debug|trace`, default `trace` in debug builds, `warn` in release).

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Default logging filter level depending on the build profile.
///
/// Returns `log::LevelFilter::Trace` in debug builds (`cfg!(debug_assertions)` is true)
/// to capture detailed diagnostic information, and `log::LevelFilter::Warn` in release
/// builds to minimize logging overhead and noise.
pub fn default_level() -> log::LevelFilter {
    default_level_for(cfg!(debug_assertions))
}

/// Pure helper for `default_level` taking an explicit `debug_assertions` flag,
/// allowing both profile defaults to be unit tested directly.
pub fn default_level_for(debug_assertions: bool) -> log::LevelFilter {
    if debug_assertions {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Warn
    }
}

/// Level applied to records from crates that are not ours. `trace` in a debug
/// build means tokio, mio, rustls and hyper each log on every event-loop turn,
/// which buries our own records and writes far more than it is worth; their
/// warnings and errors are still kept, because those are the ones worth seeing.
const DEPENDENCY_LEVEL: log::LevelFilter = log::LevelFilter::Warn;

/// Whether a record's target belongs to this workspace. Every crate here is
/// named `pacode…`, and `log` targets default to the module path.
fn is_own_target(target: &str) -> bool {
    target.starts_with("pacode")
}

/// File logger backend implementing `log::Log`.
pub struct FileLogger {
    file: Mutex<File>,
    level: log::LevelFilter,
}

impl FileLogger {
    pub fn new(file: File, level: log::LevelFilter) -> Self {
        Self {
            file: Mutex::new(file),
            level,
        }
    }

    /// The level a target is held to: the configured one for our own crates,
    /// `DEPENDENCY_LEVEL` for everything else — but never above what was asked
    /// for, so `PACODE_LOG=error` stays quiet across the board.
    fn level_for(&self, target: &str) -> log::LevelFilter {
        if is_own_target(target) {
            self.level
        } else {
            self.level.min(DEPENDENCY_LEVEL)
        }
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let level = record.level();
        let target = record.target();
        let args = record.args();
        let line = format!("{ts} {level:<5} {target}: {args}\n");
        let mut file = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = file.write_all(line.as_bytes());
    }

    fn flush(&self) {
        let mut file = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = file.flush();
    }
}

/// Install the global logger writing to `path`. Calling twice is a no-op.
/// Format: `2026-09-09T12:34:56.789Z INFO  target: message`.
pub fn init_file_logger(path: &Path, level: log::LevelFilter) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    let logger = FileLogger::new(file, level);
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
    Ok(())
}

/// Parse `PACODE_LOG`; `None` when unset or invalid.
pub fn level_from_env(value: Option<&str>) -> Option<log::LevelFilter> {
    let s = value?.trim();
    match s.to_ascii_lowercase().as_str() {
        "error" => Some(log::LevelFilter::Error),
        "warn" => Some(log::LevelFilter::Warn),
        "info" => Some(log::LevelFilter::Info),
        "debug" => Some(log::LevelFilter::Debug),
        "trace" => Some(log::LevelFilter::Trace),
        "off" => Some(log::LevelFilter::Off),
        _ => None,
    }
}
