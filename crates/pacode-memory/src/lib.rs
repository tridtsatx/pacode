//! Agent memory: shared (global) and per-project stores in SQLite with FTS5 recall.
//!
//! - [`MemoryStore`]: write (with near-duplicate update), recall (BM25 x recency x pin x
//!   recall-count), forget (soft delete, purged after 90 days at open), list, prompt snapshot.
//! - [`inject`]: renders the compact system-prompt section (pinned in full + index).
//! - [`embed`]: optional, off-by-default semantic layer (trait + hybrid scoring only).
//! - Legacy `memory.md` files are imported once on first open ([`legacy`]).

mod db;
pub mod embed;
mod error;
mod hybrid;
mod id;
pub mod inject;
pub mod legacy;
mod ops;
pub mod rank;
mod schema;
mod store;
pub mod text;
mod types;

pub use embed::Embedder;
pub use error::MemoryError;
pub use id::new_id;
pub use inject::{Snapshot, render as render_prompt_section};
pub use store::{MemoryStore, global_db_path, project_db_path};
pub use types::*;

#[cfg(test)]
mod embed_tests;
#[cfg(test)]
mod store_tests;
