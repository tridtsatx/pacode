//! Tree-sitter symbol extraction: language detection, per-language queries and
//! the outline builder used by the `symbols` tool. Pure logic — no IO, no tokio;
//! [`extract`] is synchronous CPU work that callers run inside `spawn_blocking`.

pub mod extract;
pub mod language;
pub mod queries;

pub use extract::{
    ExtractOptions, FileSymbols, MAX_PARSE_BYTES, Symbol, SymbolKind, SymbolsError, extract,
};
pub use language::Language;
