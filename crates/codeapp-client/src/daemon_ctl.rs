//! Daemon lifecycle helpers used by `codeapp daemon status|stop` and by autospawn.

use std::path::{Path, PathBuf};

use crate::ClientError;

#[derive(Clone, Debug, PartialEq)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub socket: PathBuf,
}

/// Spawn `<exe> serve --detach [--socket <path>]` fully detached (`setsid`, stdio to
/// `log_file`), return the child pid. Does not wait for the socket.
pub fn spawn_daemon(
    exe: &Path,
    socket: &Path,
    log_file: &Path,
    extra_args: &[String],
) -> Result<u32, ClientError> {
    let _ = (exe, socket, log_file, extra_args);
    todo!("daemon_ctl::spawn_daemon")
}

/// Connect, `Hello`, `Ping`, close. `None` when no daemon answers.
pub async fn daemon_status(socket: &Path, app_version: &str) -> Option<DaemonStatus> {
    let _ = (socket, app_version);
    todo!("daemon_ctl::daemon_status")
}

/// Send `Shutdown{force}`. Ok when the daemon acknowledged (it exits when idle, or
/// immediately with `force`).
pub async fn stop_daemon(socket: &Path, app_version: &str, force: bool) -> Result<(), ClientError> {
    let _ = (socket, app_version, force);
    todo!("daemon_ctl::stop_daemon")
}

/// Poll until the socket accepts a connection or `timeout` elapses.
pub async fn wait_for_socket(socket: &Path, timeout: std::time::Duration) -> bool {
    let _ = (socket, timeout);
    todo!("daemon_ctl::wait_for_socket")
}
