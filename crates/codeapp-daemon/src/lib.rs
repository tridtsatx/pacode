//! The daemon: unix socket server in front of [`codeapp_core::Core`] (spec §3, §5).
//!
//! - [`build_core`]: wire config → providers, built-in + MCP tools, TaskManager,
//!   McpPool, Store, Core.
//! - [`run`]: bind the socket (remove a stale file after a failed connect probe),
//!   write the pid file, accept connections, serve until `Shutdown`, idle timeout
//!   (`daemon.idle_timeout_secs` with no clients and `core.is_idle()`), SIGTERM/SIGINT.
//!   On exit: `core.shutdown()`, remove socket + pid file.
//! - Per connection ([`connection`]): NDJSON reader → `Envelope`; `Hello` first
//!   (protocol check), `Attach` → `core.open_session` + subscribe to its events
//!   (a task forwards `(seq, event)` as `ServerMessage::Event` into the writer
//!   channel; an `Attach` to another session replaces the subscription), `Detach`,
//!   `Ping`, `Shutdown`; everything else → `core.handle(session, req)`.
//!   Replies and events share one ordered writer channel (capacity 1024; a client
//!   that lags gets `Event` drops and must `GetSnapshot`).
//! - Text deltas are already coalesced by the core.

pub mod connection;
pub mod server;
pub mod wiring;

use std::path::PathBuf;
use std::sync::Arc;

use codeapp_config::Paths;
use codeapp_core::Core;
use codeapp_types::Config;

pub use server::run;
pub use wiring::build_core;

#[derive(Clone, Debug)]
pub struct DaemonOptions {
    pub paths: Paths,
    pub config: Arc<Config>,
    pub socket: PathBuf,
    pub app_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("socket {path} is in use by another daemon")]
    SocketBusy { path: PathBuf },
    #[error("bind {path}: {source}")]
    Bind {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("core: {0}")]
    Core(#[from] codeapp_core::CoreError),
    #[error("provider: {0}")]
    Provider(#[from] codeapp_provider::ProviderError),
    #[error("store: {0}")]
    Store(#[from] codeapp_store::StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Number of connected clients + whether any is attached; exposed for tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerStats {
    pub connections: usize,
}

#[allow(dead_code)]
fn _uses(_: Arc<Core>) {}
