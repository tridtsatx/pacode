//! Evaluating a monitor's condition.
//!
//! Every check is bounded: a command gets [`super::CHECK_TIMEOUT_SECS`] and a file
//! read gets a byte cap, so a hung process or a growing log cannot make the
//! scheduler stall or the memory grow.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pacode_types::time::now_ms;
use pacode_types::{Event, MonitorCondition, MonitorId, MonitorInfo, MonitorStatus};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::session::Session;

use super::Scheduler;

/// Most bytes read from a watched file before the match is decided.
pub const FILE_READ_CAP_BYTES: usize = 256 * 1024;

/// Why a check could not be decided. A failure is not a match: a monitor whose
/// check errors is marked failed rather than silently firing.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("check timed out after {0}s")]
    Timeout(u64),
    #[error("failed to run the check: {0}")]
    Spawn(String),
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
}

pub(super) struct MonitorCheck {
    pub(super) info: MonitorInfo,
    pub(super) cancel: CancellationToken,
}

pub(super) fn retain_terminal(monitors: &mut Vec<MonitorInfo>, preserve: Option<&MonitorId>) {
    let mut excess = monitors
        .iter()
        .filter(|monitor| monitor.status.is_terminal())
        .count()
        .saturating_sub(super::TERMINAL_MONITOR_RETENTION);
    if excess == 0 {
        return;
    }
    let preserve = preserve.cloned();
    monitors.retain(|monitor| {
        let protected = preserve
            .as_ref()
            .is_some_and(|preserve| &monitor.id == preserve);
        if excess > 0 && monitor.status.is_terminal() && !protected {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

pub(super) fn due_checks(scheduler: &Scheduler, now: u64) -> Vec<MonitorCheck> {
    let monitors = scheduler
        .monitors
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut cancels = scheduler
        .monitor_cancels
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    monitors
        .iter()
        .filter(|monitor| monitor.status.is_live())
        .filter(|monitor| monitor.next_check_at_ms().is_none_or(|next| next <= now))
        .map(|monitor| MonitorCheck {
            info: monitor.clone(),
            cancel: cancels.entry(monitor.id.clone()).or_default().clone(),
        })
        .collect()
}

pub(super) fn record_check(
    scheduler: &Scheduler,
    id: &MonitorId,
    now: u64,
    fired: bool,
) -> Option<MonitorInfo> {
    let updated = {
        let mut monitors = scheduler
            .monitors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let monitor = monitors.iter_mut().find(|monitor| &monitor.id == id)?;
        if !monitor.status.is_live() {
            return None;
        }
        monitor.last_check_ms = Some(now);
        if fired {
            monitor.status = MonitorStatus::Fired;
            monitor.fired_at_ms = Some(now);
        }
        let updated = monitor.clone();
        if fired {
            retain_terminal(&mut monitors, Some(id));
        }
        updated
    };
    if updated.status.is_terminal() {
        cancel(scheduler, id);
    }
    Some(updated)
}

pub(super) fn record_failure(
    scheduler: &Scheduler,
    id: &MonitorId,
    now: u64,
) -> Option<MonitorInfo> {
    let updated = {
        let mut monitors = scheduler
            .monitors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let monitor = monitors.iter_mut().find(|monitor| &monitor.id == id)?;
        if !monitor.status.is_live() {
            return None;
        }
        monitor.last_check_ms = Some(now);
        monitor.status = MonitorStatus::Failed;
        let updated = monitor.clone();
        retain_terminal(&mut monitors, Some(id));
        updated
    };
    cancel(scheduler, id);
    Some(updated)
}

pub(super) fn cancel(scheduler: &Scheduler, id: &MonitorId) {
    if let Some(cancel) = scheduler
        .monitor_cancels
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(id)
    {
        cancel.cancel();
    }
}

/// Whether the condition holds right now.
pub async fn check_condition(condition: &MonitorCondition, cwd: &Path) -> Result<bool, CheckError> {
    let timeout = Duration::from_secs(super::CHECK_TIMEOUT_SECS);
    match condition {
        MonitorCondition::CommandSucceeds { command } => {
            run_shell(command, cwd).await.map(|code| code == 0)
        }
        MonitorCondition::ProcessGone { pattern } => {
            let command = format!("pgrep -f -- {}", shell_quote(pattern));
            run_shell(&command, cwd).await.map(|code| code != 0)
        }
        MonitorCondition::FileExists { path } => {
            let path = resolve(cwd, path);
            with_file_check_timeout(timeout, async {
                Ok(tokio::fs::metadata(&path).await.is_ok())
            })
            .await
        }
        MonitorCondition::FileMatches { path, pattern } => {
            let path = resolve(cwd, path);
            let error_path = path.display().to_string();
            with_file_check_timeout(timeout, async {
                let mut file = match tokio::fs::File::open(&path).await {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(false);
                    }
                    Err(source) => {
                        return Err(CheckError::Read {
                            path: error_path,
                            source,
                        });
                    }
                };
                let mut buf = vec![0u8; FILE_READ_CAP_BYTES];
                let read = file
                    .read(&mut buf)
                    .await
                    .map_err(|source| CheckError::Read {
                        path: error_path,
                        source,
                    })?;
                buf.truncate(read);
                Ok(String::from_utf8_lossy(&buf).contains(pattern.as_str()))
            })
            .await
        }
    }
}

pub(super) async fn with_file_check_timeout<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, CheckError>>,
) -> Result<T, CheckError> {
    let timeout_secs = timeout.as_secs();
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result,
        Err(_) => Err(CheckError::Timeout(timeout_secs)),
    }
}

pub(super) async fn fire(scheduler: &Scheduler, session: &Arc<Session>, check: MonitorCheck) {
    let MonitorCheck {
        info,
        cancel: cancellation,
    } = check;
    let cwd = session.meta().cwd;
    let outcome = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return,
        outcome = check_condition(&info.condition, &cwd) => outcome,
    };
    let now = now_ms();
    let updated = match outcome {
        Ok(true) => {
            let updated = scheduler.record_check(&info.id, now, true);
            if let Some(updated) = &updated {
                session.inject_monitor_fired(updated);
            }
            updated
        }
        Ok(false) => scheduler.record_check(&info.id, now, false),
        Err(error) => {
            log::warn!("monitor {} check failed: {error}", info.id);
            scheduler.record_failure(&info.id, now)
        }
    };
    if let Some(updated) = updated {
        session.events.emit(Event::MonitorUpdated(updated));
    }
}

fn resolve(cwd: &Path, path: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Single-quote a value for `sh -c`, so a pattern cannot become a second command.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

async fn run_shell(command: &str, cwd: &Path) -> Result<i32, CheckError> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(|e| CheckError::Spawn(e.to_string()))?;
    let timeout = Duration::from_secs(super::CHECK_TIMEOUT_SECS);
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status.code().unwrap_or(-1)),
        Ok(Err(e)) => Err(CheckError::Spawn(e.to_string())),
        Err(_) => {
            // A check that outlives its budget is killed, not left behind.
            let _ = child.kill().await;
            Err(CheckError::Timeout(super::CHECK_TIMEOUT_SECS))
        }
    }
}
