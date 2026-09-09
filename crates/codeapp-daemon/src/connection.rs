//! One client connection.

use std::sync::Arc;

use codeapp_core::Core;
use tokio::net::UnixStream;

/// Shared control handle: shutdown request + connection counter.
#[derive(Clone)]
pub struct ServerControl {
    pub shutdown: tokio_util::sync::CancellationToken,
    pub connections: Arc<std::sync::atomic::AtomicUsize>,
    pub app_version: String,
    pub pid: u32,
    /// Set by `Shutdown{force:false}`: exit as soon as the core is idle.
    pub shutdown_when_idle: Arc<std::sync::atomic::AtomicBool>,
}

/// Serve one connection until EOF or shutdown.
pub async fn serve_connection(stream: UnixStream, core: Arc<Core>, control: ServerControl) {
    let _ = (stream, core, control);
    todo!("connection::serve_connection")
}
