//! Model-facing tool descriptions, one plain-text file per tool.
//!
//! The files in `prompts/` are compiled into the binaries with `include_str!`
//! from the matching `builtin/<tool>.rs`, so the text a tool shows the model is
//! edited as prose in its own file, not as a string literal wedged between the
//! `impl Tool` blocks. A file holds *only* the description: no heading, no
//! markdown wrapper — it is inserted verbatim into the tool definition.
//!
//! Kept for the convention check in `prompts_tests.rs`; nothing is re-exported
//! at runtime, the descriptions are inlined at compile time.

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod prompts_tests;
