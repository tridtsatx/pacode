//! Accept loop, idle timeout, signals.

use std::sync::Arc;

use codeapp_core::Core;

use crate::{DaemonError, DaemonOptions};

/// Serve until shutdown. Returns after cleanup.
pub async fn run(opts: DaemonOptions, core: Arc<Core>) -> Result<(), DaemonError> {
    let _ = (opts, core);
    todo!("server::run")
}

/// Bind the listener: if the socket file exists and a connect attempt succeeds →
/// `SocketBusy`; if it exists but refuses → remove and bind; set mode 0600.
pub async fn bind_socket(path: &std::path::Path) -> Result<tokio::net::UnixListener, DaemonError> {
    let _ = path;
    todo!("server::bind_socket")
}
