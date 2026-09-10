//! Platform-native detached web browser launcher.

use std::process::{Command, Stdio};

use crate::error::{AuthError, Result};

/// Build the platform-specific [`Command`] for launching `url`.
pub fn build_browser_command(url: &str) -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "start", "", url]);
        cmd
    }

    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    }
}

/// Open `url` in the system's default web browser.
///
/// Spawns the system opener detached with `stdin`, `stdout`, and `stderr` redirected
/// to null. Never blocks and never panics. Returns [`AuthError::Io`] if the opener command
/// fails to spawn (e.g. missing binary or headless environment), allowing the caller
/// to display the URL in the terminal instead.
pub fn open_url(url: &str) -> Result<()> {
    let mut cmd = build_browser_command(url);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd.spawn().map_err(AuthError::Io)?;
    drop(child);

    Ok(())
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod browser_tests;
