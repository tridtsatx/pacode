//! Watches the configuration file using kernel events (inotify/kqueue/etc.) with trailing-edge debouncing.
//!
//! Handles multi-step atomic writes (write-to-temp + rename-in-place) by watching the parent directory
//! non-recursively and filtering for the configuration file name.

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod watcher_tests;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pacode_types::Config;

use crate::{ConfigError, load_from_path};

/// Determines whether a notification event affects the target file name.
pub fn is_relevant_event(event: &Event, target_filename: &std::ffi::OsStr) -> bool {
    match event.kind {
        EventKind::Access(_) => false,
        _ => event
            .paths
            .iter()
            .any(|p| p.file_name() == Some(target_filename)),
    }
}

/// Runs a trailing-edge debounce loop on an event receiver.
///
/// Multi-step writes resetting the deadline within `debounce` will settle
/// before triggering `on_settle` exactly once.
pub async fn run_debounced<E>(
    mut rx: tokio::sync::mpsc::Receiver<E>,
    debounce: Duration,
    is_relevant: impl Fn(&E) -> bool,
    mut on_settle: impl FnMut(),
) {
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => {
                        if is_relevant(&ev) {
                            deadline = Some(tokio::time::Instant::now() + debounce);
                        }
                    }
                    None => {
                        if deadline.is_some() {
                            on_settle();
                        }
                        break;
                    }
                }
            }
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                deadline = None;
                on_settle();
            }
        }
    }
}

/// Watches a configuration file and invokes a callback with the reloaded `Config` or `ConfigError`.
pub struct ConfigWatcher {
    _watcher: Option<RecommendedWatcher>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Watch `path` for modifications, renames, and creations with kernel events and a debounce window.
    pub fn watch<F>(path: PathBuf, debounce: Duration, on_change: F) -> Result<Self, ConfigError>
    where
        F: Fn(Result<Config, ConfigError>) + Send + Sync + 'static,
    {
        let parent_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        if !parent_dir.exists() {
            let _ = std::fs::create_dir_all(&parent_dir);
        }

        let target_filename: OsString = path
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .unwrap_or_else(|| OsString::from("config.toml"));

        let (tx, rx) = tokio::sync::mpsc::channel::<Event>(100);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            notify::Config::default(),
        )
        .map_err(|e| ConfigError::Read {
            path: parent_dir.clone(),
            source: std::io::Error::other(e.to_string()),
        })?;

        watcher
            .watch(&parent_dir, RecursiveMode::NonRecursive)
            .map_err(|e| ConfigError::Read {
                path: parent_dir.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;

        let on_change = Arc::new(on_change);
        let config_path = path.clone();

        let task = tokio::spawn(async move {
            run_debounced(
                rx,
                debounce,
                move |ev| is_relevant_event(ev, &target_filename),
                move || {
                    let result = load_from_path(&config_path);
                    on_change(result);
                },
            )
            .await;
        });

        Ok(Self {
            _watcher: Some(watcher),
            task: Some(task),
        })
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
