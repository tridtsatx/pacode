//! Token accounting for `/analyze`: where a session's tokens actually went.
//!
//! The split is a measurement with a caveat attached, and the caveat is the
//! point. pacode has no tokenizer; the only real number a provider hands back
//! is a per-request `input_tokens`/`output_tokens` pair. Everything finer than
//! that is pacode's chars/4 estimator — the same one the context window and
//! the compaction threshold use — so the page labels those rows an estimate
//! rather than printing a precise-looking integer built from a character count.
//!
//! - [`attribution`]: one request → one [`RequestAttribution`]
//! - [`ledger`]: the session-long [`SessionAccounting`] every request and tool
//!   result is charged to, under explicit caps
//!
//! The report the page draws ([`pacode_types::AnalyzeReport`]) lives in
//! `pacode-types` rather than here: the daemon fills it and the client renders
//! it, and those two crates do not depend on each other.

pub mod attribution;
pub mod ledger;

pub use attribution::RequestAttribution;
pub use ledger::{RequestLatency, SessionAccounting};

#[cfg(test)]
#[path = "analyze_tests.rs"]
mod analyze_tests;
