//! Tiny file logger for the `log` facade. No timers, no background thread: each
//! record is formatted and appended under a mutex. Level from `PACODE_LOG`
//! (`error|warn|info|debug|trace`, default `info`).

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

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
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
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
