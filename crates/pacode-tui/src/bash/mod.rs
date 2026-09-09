//! Client-side bash mode execution (`!` prompt prefix).
//!
//! # Architecture Decision
//! The command is executed by the CLIENT, locally — its output goes to the TUI only
//! and never into the model's context. This is a deliberate decision, not an oversight.
//! The model never sees bash commands or their outputs, preserving context budget
//! and maintaining strict separation between client operations and model turns.

pub mod complete;
pub mod highlight;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

use std::path::Path;

pub use complete::{BASH_HISTORY_CAP, Candidate, complete, record_history};
pub use highlight::{BashSpan, tokenize};

/// Known interactive or full-screen terminal programs that require full terminal takeover
/// via `terminal::run_suspended`.
pub const INTERACTIVE_PROGRAMS: &[&str] = &[
    "vim", "vi", "nvim", "nano", "emacs", "kak", "top", "htop", "btop", "less", "more", "man",
    "tig", "lazygit", "tmux", "screen", "gdb", "lldb", "ssh", "fzf",
];

/// Maximum bytes kept from captured bash output. Keeps tail when exceeded.
pub const BASH_OUTPUT_CAP_BYTES: usize = 64 * 1024; // 64 KiB

/// Checks whether the prompt text is in bash mode (leading `!`).
pub fn is_bash_mode(text: &str) -> bool {
    text.starts_with('!')
}

/// Extracts the bash command string from the prompt (without the leading `!`).
pub fn command_text(text: &str) -> &str {
    text.strip_prefix('!').unwrap_or(text).trim()
}

/// Decides whether a command should take over the terminal interactively.
///
/// Rule:
/// 1. If the user prefixed the command with an extra `!` (e.g. `!!my_command`), it requests
///    interactive execution.
/// 2. If the first word (program basename) matches `INTERACTIVE_PROGRAMS`.
pub fn is_interactive(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.starts_with('!') {
        return true;
    }
    let first_word = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('/')
        .next_back()
        .unwrap_or("");

    INTERACTIVE_PROGRAMS.contains(&first_word)
}

/// Result of a locally executed bash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashOutput {
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

/// Truncate raw process output keeping the tail.
pub fn truncate_output(bytes: &[u8], cap_bytes: usize) -> (String, bool) {
    if bytes.len() <= cap_bytes {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    } else {
        let tail_bytes = &bytes[bytes.len() - cap_bytes..];
        let tail_str = String::from_utf8_lossy(tail_bytes);
        let note = format!("[output truncated: kept last {cap_bytes} bytes]\n");
        (format!("{note}{tail_str}"), true)
    }
}

/// Get the user's shell: `$SHELL`, falling back to `/bin/sh`.
pub fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Run a command captured asynchronously (used by background task in `app.rs`).
pub async fn run_captured(
    cmd: &str,
    cwd: &Path,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<BashOutput, String> {
    let shell = user_shell();
    let mut command = tokio::process::Command::new(&shell);
    command.arg("-c").arg(cmd).current_dir(cwd);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {shell}: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = tokio::spawn(async move {
        if let Some(mut r) = stdout {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        }
    });

    let stderr_handle = tokio::spawn(async move {
        if let Some(mut r) = stderr {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        }
    });

    tokio::select! {
        res = child.wait() => {
            let status = res.map_err(|e| format!("child wait failed: {e}"))?;
            let stdout_bytes = stdout_handle.await.unwrap_or_default();
            let stderr_bytes = stderr_handle.await.unwrap_or_default();

            let mut combined = stdout_bytes;
            combined.extend_from_slice(&stderr_bytes);

            let (output, truncated) = truncate_output(&combined, BASH_OUTPUT_CAP_BYTES);
            Ok(BashOutput {
                command: cmd.to_string(),
                output,
                exit_code: status.code(),
                truncated,
            })
        }
        _ = &mut cancel_rx => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok(BashOutput {
                command: cmd.to_string(),
                output: "[cancelled by user]\n".to_string(),
                exit_code: Some(130), // standard 128 + SIGINT
                truncated: false,
            })
        }
    }
}
