//! Evaluating a monitor's condition.
//!
//! Every check is bounded: a command gets [`super::CHECK_TIMEOUT_SECS`] and a file
//! read gets a byte cap, so a hung process or a growing log cannot make the
//! scheduler stall or the memory grow.

use std::path::Path;
use std::time::Duration;

use pacode_types::MonitorCondition;
use tokio::io::AsyncReadExt;

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

/// Whether the condition holds right now.
pub async fn check_condition(condition: &MonitorCondition, cwd: &Path) -> Result<bool, CheckError> {
    match condition {
        MonitorCondition::CommandSucceeds { command } => {
            run_shell(command, cwd).await.map(|code| code == 0)
        }
        MonitorCondition::ProcessGone { pattern } => {
            // `pgrep` exits 1 when nothing matches, which is exactly "gone".
            let command = format!("pgrep -f -- {}", shell_quote(pattern));
            run_shell(&command, cwd).await.map(|code| code != 0)
        }
        MonitorCondition::FileExists { path } => {
            let path = resolve(cwd, path);
            Ok(tokio::fs::metadata(&path).await.is_ok())
        }
        MonitorCondition::FileMatches { path, pattern } => {
            let path = resolve(cwd, path);
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(f) => f,
                // A file that is not there yet simply has not matched yet.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(source) => {
                    return Err(CheckError::Read {
                        path: path.display().to_string(),
                        source,
                    });
                }
            };
            let mut buf = vec![0u8; FILE_READ_CAP_BYTES];
            let read = file
                .read(&mut buf)
                .await
                .map_err(|source| CheckError::Read {
                    path: path.display().to_string(),
                    source,
                })?;
            buf.truncate(read);
            Ok(String::from_utf8_lossy(&buf).contains(pattern.as_str()))
        }
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
        .stderr(std::process::Stdio::null());
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
