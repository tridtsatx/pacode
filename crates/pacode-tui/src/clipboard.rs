//! Clipboard support: OSC 52 sequence generation and fallback to system clipboard via `arboard`.

use std::io::Write;

pub use crate::clipboard_read::*;

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod clipboard_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Osc52,
    System,
    Both,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("stdout write failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("system clipboard failed: {0}")]
    System(String),
    #[error("all clipboard methods failed")]
    AllFailed,
}

pub const REMOTE_HINT_TEXT: &str = "Copied via OSC52. In tmux run: set -g set-clipboard on. Over ssh your terminal must allow OSC52.";

/// Encodes raw bytes as a standard base64 string.
pub fn base64_encode(bytes: &[u8]) -> String {
    const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let chunks = bytes.chunks_exact(3);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let b0 = chunk[0] as usize;
        let b1 = chunk[1] as usize;
        let b2 = chunk[2] as usize;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARSET[(n >> 18) & 63] as char);
        out.push(CHARSET[(n >> 12) & 63] as char);
        out.push(CHARSET[(n >> 6) & 63] as char);
        out.push(CHARSET[n & 63] as char);
    }

    match remainder.len() {
        1 => {
            let n = (remainder[0] as usize) << 16;
            out.push(CHARSET[(n >> 18) & 63] as char);
            out.push(CHARSET[(n >> 12) & 63] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((remainder[0] as usize) << 16) | ((remainder[1] as usize) << 8);
            out.push(CHARSET[(n >> 18) & 63] as char);
            out.push(CHARSET[(n >> 12) & 63] as char);
            out.push(CHARSET[(n >> 6) & 63] as char);
            out.push('=');
        }
        _ => {}
    }

    out
}

/// Formats the OSC 52 sequence for text, wrapping in tmux DCS passthrough when inside tmux.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> Vec<u8> {
    let b64 = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{b64}\x07");
    if in_tmux {
        format!("\x1bPtmux;\x1b{seq}\x1b\\").into_bytes()
    } else {
        seq.into_bytes()
    }
}

/// Helper for remote hint detection taking env strings as parameters.
pub fn detect_remote_hint(
    tmux: Option<&str>,
    ssh_tty: Option<&str>,
    ssh_connection: Option<&str>,
) -> Option<&'static str> {
    let has_tmux = tmux.is_some_and(|s| !s.trim().is_empty());
    let has_ssh_tty = ssh_tty.is_some_and(|s| !s.trim().is_empty());
    let has_ssh_conn = ssh_connection.is_some_and(|s| !s.trim().is_empty());

    if has_tmux || has_ssh_tty || has_ssh_conn {
        Some(REMOTE_HINT_TEXT)
    } else {
        None
    }
}

/// Returns a hint string if inside a tmux session or over an SSH connection.
pub fn remote_hint() -> Option<&'static str> {
    let tmux = std::env::var("TMUX").ok();
    let ssh_tty = std::env::var("SSH_TTY").ok();
    let ssh_conn = std::env::var("SSH_CONNECTION").ok();
    detect_remote_hint(tmux.as_deref(), ssh_tty.as_deref(), ssh_conn.as_deref())
}

/// Copies text: first writes OSC 52 to stdout; then also attempts system clipboard via `arboard`.
pub fn copy(text: &str) -> Result<Method, ClipboardError> {
    let in_tmux = std::env::var_os("TMUX").is_some();
    let seq = osc52_sequence(text, in_tmux);

    let osc52_result = (|| -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&seq)?;
        stdout.flush()?;
        Ok(())
    })();
    let osc52_ok = osc52_result.is_ok();

    let arboard_result = (|| -> Result<(), String> {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        clipboard.set_text(text).map_err(|e| e.to_string())?;
        Ok(())
    })();
    let arboard_ok = arboard_result.is_ok();

    match (osc52_ok, arboard_ok) {
        (true, true) => Ok(Method::Both),
        (true, false) => Ok(Method::Osc52),
        (false, true) => Ok(Method::System),
        (false, false) => {
            if let Err(e) = osc52_result {
                Err(ClipboardError::Io(e))
            } else if let Err(e) = arboard_result {
                Err(ClipboardError::System(e))
            } else {
                Err(ClipboardError::AllFailed)
            }
        }
    }
}
