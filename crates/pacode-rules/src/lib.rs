//! Code-style guard rules.
//!
//! Pure logic (no async, no IO beyond reading rule files): parse rule files,
//! layer built-in packs with global and project rules, and scan the lines a tool
//! call ADDED for violations. `pacode-core` wires it to the turn loop.
//!
//! # Rule file
//!
//! Markdown with frontmatter, in `<project>/.pacode/rules/*.md` or
//! `<config>/rules/*.md` (grammar: see `parse_rule`):
//!
//! ```text
//! ---
//! name: ts-no-explicit-any          # unique id; same name overrides a built-in
//! languages: [ts, tsx]              # file extensions
//! match: '(?::\s*any\b)'            # regex on each ADDED line
//! unless: '...'                     # optional: skip when this matches from the hit on
//! unless_above: '...'               # optional: skip when the comment block above matches
//! scope: code                       # code (comments/strings blanked) | any
//! severity: block                   # warn | block (block marks the result as an error)
//! mode: stream                      # after_write (default) | stream (+ after_write fallback)
//! max_per_turn: 3                   # firings per turn
//! exclude: ['*.d.ts', 'tests/']     # path patterns, see path_excluded
//! skip_test_code: true              # Rust: skip #[cfg(test)] / #[test] items
//! ---
//! Message shown to the model.
//! ```
//!
//! Precedence: built-in < global < project (by `name`); `[rules] disable` removes
//! names from every layer.

mod builtin;
mod diff;
mod limiter;
mod mask;
mod parse;
mod render;
mod rule;
mod ruleset;
mod scan;
mod stream_args;

pub use builtin::{PACKS, Pack, builtin_rules};
pub use diff::{OldLines, added_by_set, added_line_indices};
pub use limiter::{MAX_STREAM_RETRIES_PER_TURN, TurnLimiter};
pub use mask::{Syntax, mask_code, rust_test_lines};
pub use parse::parse_rule;
pub use render::{
    CLOSE_TAG, OPEN_TAG, STREAM_STOP_PREFIX, pinned_line, render_after_write, render_stream_stop,
};
pub use rule::{Mode, Origin, Rule, RuleError, Scope, Severity};
pub use ruleset::{Fingerprint, LoadOptions, Loaded, ModeFilter, RuleSet, ext_of, path_excluded};
pub use scan::{
    Finding, MAX_FINDINGS_PER_RULE, MAX_FINDINGS_TOTAL, ScanReport, scan_added, scan_added_fast,
    scan_lines,
};
pub use stream_args::{StrField, extract_string_fields};

#[cfg(test)]
mod builtin_tests;
#[cfg(test)]
mod diff_tests;
#[cfg(test)]
mod limiter_tests;
#[cfg(test)]
mod mask_tests;
#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod ruleset_tests;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod stream_args_tests;
