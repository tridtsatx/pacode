//! One-shot capture: run a program, wait with a timeout, keep a bounded prefix
//! of each stream. No task is registered and no events are emitted — unlike
//! [`crate::TaskManager`], which tracks user-visible background work, this is
//! for internal queries (`git log`, version probes) that must stay invisible.
//!
//! Shares the supervisor's spawn conventions: own process group on unix, stdin
//! null, both pipes drained concurrently, the process group is SIGKILLed on the
//! timeout and right after the child exits (never left running).

use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::manager::ExecError;

/// Result of a [`run_capture`] call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturedOutput {
    /// Bytes of stdout kept (capped at `max_output_bytes`).
    pub stdout: Vec<u8>,
    /// Bytes of stderr kept (same cap).
    pub stderr: Vec<u8>,
    /// Process exit code; `None` when killed by a signal or the wait failed.
    pub status: Option<i32>,
    /// The timeout fired and the process group was killed.
    pub timed_out: bool,
    /// stdout exceeded `max_output_bytes`; the rest was drained and dropped.
    pub stdout_truncated: bool,
    /// stderr exceeded `max_output_bytes`.
    pub stderr_truncated: bool,
}

impl CapturedOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0) && !self.timed_out
    }

    /// stderr as text, whitespace-trimmed (for error classification).
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Run `program args...` in `cwd`, collecting stdout/stderr until the process
/// exits, `timeout` elapses, or a stream exceeds `max_output_bytes` (kept bytes
/// stay capped; the stream is still drained so the child never blocks on a full
/// pipe).
pub async fn run_capture(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<CapturedOutput, ExecError> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(ExecError::Spawn)?;
    let child_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_reader = tokio::spawn(read_bounded(stdout, max_output_bytes));
    let stderr_reader = tokio::spawn(read_bounded(stderr, max_output_bytes));

    let mut timed_out = false;
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(res) => {
            crate::supervisor::force_kill(child_pid);
            res.ok().and_then(|s| s.code())
        }
        Err(_) => {
            timed_out = true;
            kill_group(&mut child, child_pid).await;
            None
        }
    };

    let drain = tokio::time::timeout(crate::supervisor::DRAIN_GRACE, async {
        let out = stdout_reader.await.unwrap_or_default();
        let err = stderr_reader.await.unwrap_or_default();
        (out, err)
    })
    .await;
    let ((stdout, stdout_truncated), (stderr, stderr_truncated)) = drain.unwrap_or_default();

    Ok(CapturedOutput {
        stdout,
        stderr,
        status,
        timed_out,
        stdout_truncated,
        stderr_truncated,
    })
}

/// Read a pipe to its end, keeping at most `cap` bytes; returns
/// `(kept_bytes, truncated)`.
async fn read_bounded(
    pipe: Option<impl tokio::io::AsyncRead + Unpin>,
    cap: usize,
) -> (Vec<u8>, bool) {
    let Some(mut pipe) = pipe else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if truncated {
                    continue;
                }
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                if n > room {
                    truncated = true;
                }
            }
        }
    }
    (kept, truncated)
}

/// SIGKILL the whole process group (the supervisor's timeout semantics); falls
/// back to killing just the child when the group signal is unavailable.
async fn kill_group(child: &mut tokio::process::Child, child_pid: Option<u32>) {
    crate::supervisor::force_kill(child_pid);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
#[path = "oneshot_tests.rs"]
mod oneshot_tests;
