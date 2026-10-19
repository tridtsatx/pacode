use std::process::ExitStatus;
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

    /// Record an emit made outside the gate (a forced or final one).
    fn mark_emitted(&mut self, now: Instant) {
        self.pending = false;
        self.last_emit = Some(now);
    }
}

/// Minimum spacing between two `Output` broadcasts for one task (F6). One event per
/// read chunk (8 KiB at most) lags the event channel at a hundred concurrent builds;
/// the text in between is held and sent together, never dropped.
pub(crate) const OUTPUT_EMIT_INTERVAL: Duration = Duration::from_millis(200);
/// Held live text at or beyond this size is sent at once, whatever the interval, so a
/// fast producer holds about this much per task in RAM.
pub(crate) const OUTPUT_EMIT_MAX_BYTES: usize = 64 * 1024;

/// Per-task coalescing of `TaskEvent::Output` text: the first batch goes out at once,
/// later ones at most every [`OUTPUT_EMIT_INTERVAL`], with a trailing flush when output
/// stops inside the window and a final one before `Ended`.
#[derive(Debug)]
pub(crate) struct OutputCoalescer {
    gate: ProgressThrottle,
    held: String,
    max_bytes: usize,
}

impl OutputCoalescer {
    pub(crate) fn new(interval: Duration, max_bytes: usize) -> Self {
        Self {
            gate: ProgressThrottle::new(interval),
            held: String::new(),
            max_bytes,
        }
    }

    /// Add completed lines; returns the text to broadcast now, if any.
    pub(crate) fn push(&mut self, text: &str, now: Instant) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        self.held.push_str(text);
        if self.gate.on_progress(now) {
            return Some(std::mem::take(&mut self.held));
        }
        if self.held.len() >= self.max_bytes {
            self.gate.mark_emitted(now);
            return Some(std::mem::take(&mut self.held));
        }
        None
    }

    /// When held text is owed a trailing emit, the instant it becomes due.
    pub(crate) fn flush_deadline(&self) -> Option<Instant> {
        if self.held.is_empty() {
            None
        } else {
            self.gate.flush_deadline()
        }
    }

    /// Called when the trailing deadline fires; returns the held text when it is due.
    pub(crate) fn on_flush(&mut self, now: Instant) -> Option<String> {
        if self.gate.on_flush(now) && !self.held.is_empty() {
            return Some(std::mem::take(&mut self.held));
        }
        None
    }

    /// Everything still held (the task is ending).
    pub(crate) fn take_rest(&mut self) -> Option<String> {
        if self.held.is_empty() {
            return None;
        }
        self.gate.mark_emitted(Instant::now());
        Some(std::mem::take(&mut self.held))
    }
}

/// Broadcast live output text of a task, if it still exists.
fn emit_output(id: &TaskId, state: &Arc<SharedState>, text: String) {
    let entry = state
        .tasks
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|entry| (entry.info.clone(), entry.call.clone()));
    if let Some((info, call)) = entry {
        let _ = state.events.send(TaskEvent::Output { info, call, text });
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

pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(3);

pub(crate) enum Signal {
    Terminate,
    Kill,
}

pub(crate) fn terminate(child_pid: Option<u32>) {
    signal(child_pid, Signal::Terminate);
}

pub(crate) fn force_kill(child_pid: Option<u32>) {
    signal(child_pid, Signal::Kill);
}

pub(crate) fn terminal_status(
    kill_requested: bool,
    wait_failed: bool,
    exit_code: Option<i32>,
) -> TaskStatus {
    if kill_requested {
        return TaskStatus::Killed;
    }
    if wait_failed {
        return TaskStatus::Failed;
    }
    match exit_code {
        Some(0) => TaskStatus::Completed,
        Some(_) | None => TaskStatus::Failed,
    }
}

pub(crate) async fn run_supervisor(
    id: TaskId,
    mut child: tokio::process::Child,
    mut spool_file: tokio::fs::File,
    timeout: Option<Duration>,
    mut kill_rx: tokio::sync::watch::Receiver<bool>,
    state: Arc<SharedState>,
    permit: SlotPermit,
) {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let child_pid = child.id();
    let kill_grace = Duration::from_secs(state.config.kill_grace_secs);
    let mut kill_requested = *kill_rx.borrow_and_update();
    let mut kill_deadline: Option<tokio::time::Instant> = None;
    if kill_requested {
        arm_kill(child_pid, kill_grace, &mut kill_deadline);
    }

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
    let mut child_status: Option<ExitStatus> = None;
    let mut wait_failed = false;
    let mut drain_deadline: Option<tokio::time::Instant> = None;
    let mut progress_throttle = ProgressThrottle::new(PROGRESS_EMIT_INTERVAL);
    let mut output = OutputCoalescer::new(OUTPUT_EMIT_INTERVAL, OUTPUT_EMIT_MAX_BYTES);

    loop {
        if stdout_done && stderr_done && child_waited {
            break;
        }

        let flush_at = match (progress_throttle.flush_deadline(), output.flush_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .map(tokio::time::Instant::from_std);
        tokio::select! {
            _ = async {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            }, if flush_at.is_some() => {
                let now = Instant::now();
                if progress_throttle.flush_deadline().is_some_and(|at| at <= now)
                    && progress_throttle.on_flush(now)
                {
                    emit_progress_snapshot(&id, &state);
                }
                if output.flush_deadline().is_some_and(|at| at <= now)
                    && let Some(text) = output.on_flush(now)
                {
                    emit_output(&id, &state, text);
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
                            &mut output,
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
                            &mut output,
                        ).await;
                    }
                }
            }

            _ = kill_rx.changed(), if !kill_requested => {
                kill_requested = true;
                arm_kill(child_pid, kill_grace, &mut kill_deadline);
            }

            _ = wait_for_deadline(kill_deadline), if kill_deadline.is_some() => {
                force_kill(child_pid);
                kill_deadline = None;
            }

            _ = wait_for_deadline(drain_deadline) => {
                break;
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
                kill_deadline = None;
                force_kill(child_pid);
            }

            wait_res = child.wait(), if !child_waited => {
                child_waited = true;
                match wait_res {
                    Ok(status) => child_status = Some(status),
                    Err(_) => wait_failed = true,
                }
                drain_deadline = Some(tokio::time::Instant::now() + DRAIN_GRACE);
                force_kill(child_pid);
            }
        }
    }

    if !child_waited {
        match child.wait().await {
            Ok(status) => child_status = Some(status),
            Err(_) => wait_failed = true,
        }
    }
    force_kill(child_pid);

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
    drop(spool_file);

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

    // Held live output goes out before `Ended`, never dropped.
    if let Some(text) = output.take_rest() {
        emit_output(&id, &state, text);
    }

    let exit_code = child_status.and_then(|s| s.code());
    let now_timestamp = now_ms();

    state.guard.process_ended();
    // Free the process slot (F4) before the task turns terminal, so a spawn that
    // sees it ended (and a queued spawn) finds the slot free.
    drop(permit);

    let mut ended = false;
    let evicted = {
        let mut tasks = state
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let ended_info = if let Some(entry) = tasks.get_mut(&id) {
            for line in remaining_lines {
                if let Some(p) = entry.parser.feed_line(&line, now_timestamp)
                    && entry.info.progress.as_ref().map(|progress| progress.source)
                        != Some(ProgressSource::Reported)
                {
                    entry.info.progress = Some(p);
                }
            }
            entry.info.warnings = entry.parser.warnings();
            entry.info.errors = entry.parser.errors();
            if wait_failed {
                entry.info.errors = entry.info.errors.saturating_add(1);
            }

            let status = terminal_status(entry.kill_requested, wait_failed, exit_code);
            entry.info.status = status;
            entry.info.exit_code = exit_code;
            entry.info.ended_at_ms = Some(now_timestamp);
            entry.info.output_bytes = entry.buffer.total_bytes();
            entry.child_pid = None;
            entry.version_tx.send_modify(|version| *version += 1);
            Some(entry.info.clone())
        } else {
            None
        };
        // Sent under the lock (a broadcast send never blocks): whoever sees
        // the task terminal under this lock afterwards — a dropped
        // `ForegroundGuard` sending `Unreported` — is ordered after `Ended`.
        if let Some(info) = ended_info {
            ended = true;
            let _ = state.events.send(TaskEvent::Ended(info));
        }
        state.prune_terminal_overflow(&mut tasks)
    };
    drop(evicted);
    // F5: an occasional sweep of spools no task holds any more.
    if ended {
        crate::spool::on_ended(&state);
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
    output: &mut OutputCoalescer,
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

    // The live tail, before the parser sees the lines: this is the output the
    // client draws, and the parser's job is to summarise progress out of it.
    let live_text = if complete_lines.is_empty() {
        String::new()
    } else {
        let mut text = complete_lines.join("\n");
        text.push('\n');
        text
    };

    let now_timestamp = now_ms();
    let mut progress_to_emit = None;
    let mut output_to_emit = None;

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
            if let Some(text) = output.push(&live_text, Instant::now()) {
                output_to_emit = Some((entry.info.clone(), entry.call.clone(), text));
            }
        }
    }

    if let Some(info) = progress_to_emit {
        let _ = state.events.send(TaskEvent::Progress(info));
    }
    if let Some((info, call, text)) = output_to_emit {
        let _ = state.events.send(TaskEvent::Output { info, call, text });
    }
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn arm_kill(pid: Option<u32>, grace: Duration, deadline: &mut Option<tokio::time::Instant>) {
    terminate(pid);
    if grace.is_zero() {
        force_kill(pid);
    } else {
        *deadline = Some(tokio::time::Instant::now() + grace);
    }
}

fn signal(child_pid: Option<u32>, request: Signal) {
    let Some(child_pid) = child_pid else {
        return;
    };
    let Ok(raw) = i32::try_from(child_pid) else {
        return;
    };
    #[cfg(unix)]
    {
        let number = match request {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        unsafe {
            if libc::killpg(raw, number) != 0 {
                libc::kill(raw, number);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (raw, request);
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;
