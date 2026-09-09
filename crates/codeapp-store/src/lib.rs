//! SQLite persistence (rusqlite, bundled, WAL). One writer thread owned by the daemon;
//! all access goes through [`Store`], whose methods are async wrappers over a command
//! channel to that thread, so callers never block the tokio runtime.
//!
//! Schema (spec §12): sessions, messages, agents, tasks, plans, usage, compaction,
//! messages_fts (FTS5). Migrations live in `schema.rs` and run on open.
//!
//! Submodules (to implement): `schema`, `worker`, `queries`, plus the `Store` facade here.

pub mod queries;
pub mod schema;
pub mod worker;

pub use queries::{MessageRow, SearchHit, SessionFilter};
pub use worker::Store;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store worker stopped")]
    Closed,
    #[error("not found: {0}")]
    NotFound(String),
}
