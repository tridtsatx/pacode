//! Hashline editing: content-hash line anchors (`12#ab| text`) and a pure, all-or-nothing
//! operation applier. Everything here is I/O free so other crates can call it directly.
//!
//! Model-facing flow: `read` shows every line as `LINE#HASH| text` ([`annotate`]); `edit`
//! sends [`Op`]s that name anchors instead of retyping unchanged text; [`apply_ops`]
//! verifies every anchor against the current content and either applies all ops or
//! rejects the whole edit with fresh anchored context ([`EditError::Stale`]).
//!
//! No caches live in this module: hashes are recomputed from the line text on demand.

mod anchor;
mod apply;
mod hash;
mod lines;
mod render;
mod stale;

pub use anchor::{Anchor, EditError, Op, StaleAnchor};
pub use apply::{Applied, Relocation, apply_ops};
pub use hash::{HASH_LEN, line_hash};
pub use render::{annotate, annotate_lines, annotated_line, snippet};

/// Anchors whose line number drifted are auto-relocated when exactly one line within
/// this many lines (either direction) carries the anchor's hash.
pub const RELOCATE_WINDOW: usize = 20;

/// Lines longer than this many characters are shown truncated by [`annotate`] (the hash
/// still covers the full line, so the line can be replaced whole).
pub const MAX_DISPLAY_CHARS: usize = 2000;

/// Context lines shown around each stale anchor / changed region.
pub const CONTEXT_LINES: usize = 3;

/// Cap on the lines of fresh context returned in a rejection or a success message.
pub const MAX_CONTEXT_LINES: usize = 60;

#[cfg(test)]
#[path = "apply_tests.rs"]
mod apply_tests;
#[cfg(test)]
#[path = "hash_tests.rs"]
mod hash_tests;
