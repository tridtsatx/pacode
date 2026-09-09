//! MCP (Model Context Protocol) client over stdio JSON-RPC 2.0. Port the shape of
//! jcode's `jcode-base/src/mcp/{client,manager,pool,protocol,schema_cache,tool}.rs`
//! without the shared-across-daemons pool.
//!
//! - [`McpClient`]: one server process. `initialize` handshake (protocol version
//!   `2024-11-05`, client info `codeapp`), `notifications/initialized`, `tools/list`
//!   (paginated via `nextCursor`), `tools/call`. Requests get incrementing ids and a
//!   per-request timeout; responses are matched by id; notifications and server
//!   requests are ignored (logged). stderr is drained to the log.
//! - [`McpPool`]: name → client, lazy start on first use when `lazy = true`, schema
//!   cache on disk (`cache_dir/<server>.json`, keyed by a fingerprint of the server
//!   config) so a lazy server's tools can be advertised without starting it.
//! - Tool names exposed to the model are `<server>__<tool>` ([`tool_name`]).

pub mod client;
pub mod pool;
pub mod protocol;

use std::path::PathBuf;

pub use client::McpClient;
pub use pool::McpPool;

#[derive(Clone, Debug, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpCallResult {
    /// Text content blocks joined by newlines; non-text blocks summarised as `[image]` etc.
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server '{0}' is not configured")]
    UnknownServer(String),
    #[error("failed to start '{server}': {source}")]
    Spawn {
        server: String,
        source: std::io::Error,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("request timed out after {0}s")]
    Timeout(u64),
    #[error("server error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("server exited")]
    Closed,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// `<server>__<tool>`; split back with [`split_tool_name`].
pub fn tool_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
}

pub fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    name.split_once("__")
}

/// Where the schema cache lives; `None` disables caching.
pub type CacheDir = Option<PathBuf>;
