//! Daemon connection for frontends (TUI, `pacode run`, `pacode sessions`).
//!
//! - [`Client::connect`]: connect to the socket; when it is missing/refused and
//!   `spawn_daemon` is set, run `<exe> serve --detach` and poll the socket (10 ms,
//!   up to 3 s). Sends `Hello`; a protocol mismatch is an error, a daemon version
//!   mismatch is reported in `Connected` so the caller may ask for a restart.
//! - [`Client::attach`]: bind to a session; the attach is remembered as
//!   `Attach::Resume{session}` for reconnects.
//! - Reconnect: on EOF/error the reader task emits `Disconnected`, retries with
//!   backoff 0.5 s → 8 s (`Reconnecting{attempt}`), re-sends `Hello` + `Attach`, then
//!   emits `Connected` and `Snapshot` (from the `Attached` reply). Pending requests
//!   fail with `ClientError::Disconnected`.
//! - Wire: NDJSON `Envelope` out, `ServerMessage` in; replies matched by id
//!   (`tokio::sync::oneshot`), events pushed to an `mpsc` channel (capacity 1024).

pub mod connection;
pub mod daemon_ctl;

use std::path::PathBuf;

use pacode_config::Paths;
use pacode_types::{Attach, Event, Reply, Request, SessionId, SessionSnapshot};
use tokio::sync::mpsc;

pub use connection::Client;
pub use daemon_ctl::{DaemonStatus, daemon_status, spawn_daemon, stop_daemon};

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub paths: Paths,
    /// Overrides `paths.socket_path()`.
    pub socket: Option<PathBuf>,
    pub app_version: String,
    pub spawn_daemon: bool,
    /// Executable used to spawn the daemon (default: `std::env::current_exe()`).
    pub exe: Option<PathBuf>,
    /// Request timeout.
    pub request_timeout: std::time::Duration,
}

impl ClientOptions {
    pub fn new(paths: Paths, app_version: impl Into<String>) -> Self {
        Self {
            paths,
            socket: None,
            app_version: app_version.into(),
            spawn_daemon: true,
            exe: None,
            request_timeout: std::time::Duration::from_secs(30),
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.paths.socket_path())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    Connected {
        daemon_version: String,
        pid: u32,
        /// True when the daemon runs a different binary version than this client.
        version_mismatch: bool,
    },
    Disconnected {
        reason: String,
    },
    Reconnecting {
        attempt: u32,
    },
    /// Re-attached after a reconnect; replaces all client state.
    Snapshot(SessionSnapshot),
    Event {
        seq: u64,
        event: Event,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to daemon at {path}: {source}")]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to start daemon: {0}")]
    Spawn(String),
    #[error("protocol mismatch: daemon speaks {daemon}, client {client}")]
    Protocol { daemon: u32, client: u32 },
    #[error("disconnected")]
    Disconnected,
    #[error("request timed out")]
    Timeout,
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("unexpected reply to {request}: {reply}")]
    UnexpectedReply { request: String, reply: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Typed helpers over `Client::request` used by every frontend.
pub trait ClientExt {
    fn attached_session(&self) -> Option<SessionId>;
}

/// Event receiver handed to the frontend exactly once.
pub type EventReceiver = mpsc::Receiver<ClientEvent>;

// Re-exports for frontends that only need the wire types.
pub use pacode_types::{
    Attach as AttachRequest, McpServerInfo, PluginCommandOutcome, PluginInfo, Reply as WireReply,
    Request as WireRequest,
};

#[allow(dead_code)]
fn _assert_types(_: Attach, _: Request, _: Reply) {}
