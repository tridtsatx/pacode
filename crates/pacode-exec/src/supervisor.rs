use std::sync::Arc;
use std::time::{Duration, Instant};

use pacode_types::{ProgressSource, TaskId, TaskStatus, now_ms};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::admission::SlotPermit;
use crate::manager::TaskEvent;
use crate::state::SharedState;

/// Minimum spacing between two `Progress` broadcasts for one task (F6). The stored
/// progress is always current; only the broadcast is thinned. A trailing emit fires
/// when output stops inside the window, and `Ended` carries the final state anyway.
pub(crate) const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(500);

/// Per-task gate for `TaskEvent::Progress` broadcasts.
#[derive(Debug)]
pub(crate) struct ProgressThrottle {
    interval: Duration,
    last_emit: Option<Instant>,
    pending: bool,
}

impl ProgressThrottle {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_emit: None,
            pending: false,
        }
    }

    /// Called on each new progress value. Returns true when it should be broadcast now.
    pub(crate) fn on_progress(&mut self, now: Instant) -> bool {
        match self.last_emit {
            Some(prev) if now.saturating_duration_since(prev) < self.interval => {
                self.pending = true;
                false
            }
            _ => {
                self.last_emit = Some(now);
                self.pending = false;
                true
            }
        }
    }

    /// When a trailing emit is owed, the instant it becomes due.
    pub(crate) fn flush_deadline(&self) -> Option<Instant> {
        if self.pending {
            self.last_emit.map(|t| t + self.interval)
        } else {
            None
        }
    }

    /// Called when the trailing deadline fires. Returns true when an emit is owed.
    pub(crate) fn on_flush(&mut self, now: Instant) -> bool {
        if !self.pending {
            return false;
        }
        self.pending = false;
        self.last_emit = Some(now);
        true
    }
}

/// Broadcast the current progress snapshot of a task, if it still exists.
fn emit_progress_snapshot(id: &TaskId, state: &Arc<SharedState>) {
    let info = {
        let tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.get(id).map(|e| e.info.clone())
    };
    if let Some(info) = info {
        let _ = state.events.send(TaskEvent::Progress(info));
    }
}

pub(crate) async fn run_supervisor(
    id: TaskId,
    mut child: tokio::process::Child,
    mut spool_file: tokio::fs::File,
    timeout: Option<Duration>,
    state: Arc<SharedState>,
    permit: SlotPermit,
) {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let child_pid = child.id();

    let mut stdout_buf = [0u8; 8192];
    let mut stderr_buf = [0u8; 8192];
    let mut stdout_done = stdout.is_none();
    let mut stderr_done = stderr.is_none();
    let mut stdout_line_acc = Vec::new();
    let mut stderr_line_acc = Vec::new();

    let mut spool_written = 0u64;
    let mut spool_truncated = false;
    let max_spool_bytes = state.config.max_spool_bytes;

    let stall_secs = state.config.stall_secs.max(1);
    let stall_dur = Duration::from_secs(stall_secs);
    let mut stall_interval = tokio::time::interval(stall_dur);
    stall_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    stall_interval.tick().await;

    let timeout_dur = timeout.unwrap_or(Duration::from_secs(365 * 24 * 3600));
    let timeout_sleep = tokio::time::sleep(timeout_dur);
    tokio::pin!(timeout_sleep);
    let mut timeout_active = timeout.is_some();

    let mut child_waited = false;
    let mut child_status: Option<std::process::ExitStatus> = None;
    let mut drain_deadline: Option<tokio::time::Instant> = None;
    let mut progress_throttle = ProgressThrottle::new(PROGRESS_EMIT_INTERVAL);

    loop {
        if stdout_done && stderr_done && child_waited {
            break;
        }

        if let Some(deadline) = drain_deadline
            && tokio::time::Instant::now() >= deadline
        {
            break;
        }

        let flush_at = progress_throttle
            .flush_deadline()
            .map(tokio::time::Instant::from_std);

        tokio::select! {
            _ = async {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            }, if flush_at.is_some() => {
                if progress_throttle.on_flush(Instant::now()) {
                    emit_progress_snapshot(&id, &state);
                }
            }

            res = async {
                match stdout.as_mut() {
                    Some(r) => r.read(&mut stdout_buf).await,
                    None => std::future::pending().await,
                }
            }, if !stdout_done => {
                match res {
                    Ok(0) | Err(_) => {
                        stdout_done = true;
                    }
                    Ok(n) => {
                        let chunk = &stdout_buf[..n];
                        handle_chunk(
                            &id,
                            chunk,
                            &mut spool_file,
                            &mut spool_written,
                            &mut spool_truncated,
                            max_spool_bytes,
                            &mut stdout_line_acc,
                            &state,
                            &mut progress_throttle,
                        ).await;
                    }
                }
            }

            res = async {
                match stderr.as_mut() {
                    Some(r) => r.read(&mut stderr_buf).await,
                    None => std::future::pending().await,
                }
            }, if !stderr_done => {
                match res {
                    Ok(0) | Err(_) => {
                        stderr_done = true;
                    }
                    Ok(n) => {
                        let chunk = &stderr_buf[..n];
                        handle_chunk(
                            &id,
                            chunk,
                            &mut spool_file,
                            &mut spool_written,
                            &mut spool_truncated,
                            max_spool_bytes,
                            &mut stderr_line_acc,
                            &state,
                            &mut progress_throttle,
                        ).await;
                    }
                }
            }

            _ = stall_interval.tick() => {
                let stalled_info = {
                    let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(entry) = tasks.get_mut(&id) {
                        if entry.info.status == TaskStatus::Running
                            && entry.last_activity.elapsed() >= stall_dur
                            && !entry.stalled
                        {
                            entry.stalled = true;
                            Some(entry.info.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                if let Some(info) = stalled_info {
                    let _ = state.events.send(TaskEvent::Stalled(info));
                }
            }

            _ = &mut timeout_sleep, if timeout_active => {
                timeout_active = false;
                if let Some(pid) = child_pid {
                    #[cfg(unix)]
                    unsafe {
                        libc::killpg(pid as i32, libc::SIGKILL);
                    }
                }
            }

            wait_res = child.wait(), if !child_waited => {
                child_waited = true;
                child_status = wait_res.ok();
                drain_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(3));
            }
        }
    }

    if !child_waited {
        child_status = child.wait().await.ok();
    }

    let guard_reason = {
        let tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.get(&id).and_then(|e| e.guard_reason.clone())
    };
    if let Some(reason) = guard_reason {
        let line = format!("\n{reason}\n");
        let _ = spool_file.write_all(line.as_bytes()).await;
        let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(&id) {
            entry.buffer.push(line.as_bytes());
        }
    }

    let _ = spool_file.flush().await;

    let remaining_lines: Vec<String> = {
        let mut lines = Vec::new();
        if !stdout_line_acc.is_empty() {
            let s = String::from_utf8_lossy(&stdout_line_acc).trim().to_string();
            if !s.is_empty() {
                lines.push(s);
            }
        }
        if !stderr_line_acc.is_empty() {
            let s = String::from_utf8_lossy(&stderr_line_acc).trim().to_string();
            if !s.is_empty() {
                lines.push(s);
            }
        }
        lines
    };

    let exit_code = child_status.and_then(|s| s.code());
    let now_timestamp = now_ms();

    let ended_info = {
        let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(&id) {
            for line in remaining_lines {
                if let Some(p) = entry.parser.feed_line(&line, now_timestamp)
                    && entry.info.progress.as_ref().map(|pr| pr.source)
                        != Some(ProgressSource::Reported)
                {
                    entry.info.progress = Some(p);
                }
            }
            entry.info.warnings = entry.parser.warnings();
            entry.info.errors = entry.parser.errors();

            let status = if entry.kill_requested {
                TaskStatus::Killed
            } else {
                match exit_code {
                    Some(0) => TaskStatus::Completed,
                    Some(_) | None => TaskStatus::Failed,
                }
            };
            entry.info.status = status;
            entry.info.exit_code = exit_code;
            entry.info.ended_at_ms = Some(now_timestamp);
            entry.info.output_bytes = entry.buffer.total_bytes();
            entry.version_tx.send_modify(|v| *v += 1);
            Some(entry.info.clone())
        } else {
            None
        }
    };

    state.guard.process_ended();
    // Free the process slot (F4) before announcing the end, so a queued spawn can start.
    drop(permit);
    if let Some(info) = ended_info {
        let _ = state.events.send(TaskEvent::Ended(info));
        // F1: forget the oldest finished tasks; F5: occasional spool GC.
        crate::retention::on_ended(&state, &id);
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_chunk(
    id: &TaskId,
    chunk: &[u8],
    spool_file: &mut tokio::fs::File,
    spool_written: &mut u64,
    spool_truncated: &mut bool,
    max_spool_bytes: u64,
    line_acc: &mut Vec<u8>,
    state: &Arc<SharedState>,
    progress_throttle: &mut ProgressThrottle,
) {
    if !*spool_truncated && !state.guard.spool_allowed() {
        let _ = spool_file
            .write_all(b"\n[spool stopped: low disk space]\n")
            .await;
        let _ = spool_file.flush().await;
        *spool_truncated = true;
    }
    if !*spool_truncated {
        if *spool_written + chunk.len() as u64 <= max_spool_bytes {
            let _ = spool_file.write_all(chunk).await;
            *spool_written += chunk.len() as u64;
        } else {
            let allow = max_spool_bytes.saturating_sub(*spool_written) as usize;
            if allow > 0 {
                let _ = spool_file.write_all(&chunk[..allow]).await;
                *spool_written += allow as u64;
            }
            let marker = format!("\n[spool truncated at {max_spool_bytes} bytes]\n");
            let _ = spool_file.write_all(marker.as_bytes()).await;
            *spool_truncated = true;
        }
        let _ = spool_file.flush().await;
    }

    line_acc.extend_from_slice(chunk);
    let mut complete_lines: Vec<String> = Vec::new();
    while let Some(pos) = line_acc.iter().position(|b| *b == b'\n') {
        let line_bytes: Vec<u8> = line_acc.drain(..=pos).collect();
        let mut slice = &line_bytes[..line_bytes.len() - 1];
        if slice.ends_with(b"\r") {
            slice = &slice[..slice.len() - 1];
        }
        complete_lines.push(String::from_utf8_lossy(slice).into_owned());
    }
    if line_acc.len() > 64 * 1024 {
        let dropped = std::mem::take(line_acc);
        complete_lines.push(String::from_utf8_lossy(&dropped).into_owned());
    }

    let now_timestamp = now_ms();
    let mut progress_to_emit = None;

    {
        let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.buffer.push(chunk);
            entry.info.output_bytes = entry.buffer.total_bytes();
            entry.last_activity = std::time::Instant::now();
            entry.stalled = false;

            let mut any_progress = false;
            for line in complete_lines {
                if let Some(p) = entry.parser.feed_line(&line, now_timestamp)
                    && entry.info.progress.as_ref().map(|pr| pr.source)
                        != Some(ProgressSource::Reported)
                {
                    entry.info.progress = Some(p);
                    any_progress = true;
                }
            }
            entry.info.warnings = entry.parser.warnings();
            entry.info.errors = entry.parser.errors();

            if any_progress {
                entry.version_tx.send_modify(|v| *v += 1);
                if progress_throttle.on_progress(Instant::now()) {
                    progress_to_emit = Some(entry.info.clone());
                }
            }
        }
    }

    if let Some(info) = progress_to_emit {
        let _ = state.events.send(TaskEvent::Progress(info));
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;
