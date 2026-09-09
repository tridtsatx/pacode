//! Progress and warning/error counting from process output. Only concrete patterns;
//! no guessed percentages (spec §9).
//!
//! Patterns:
//! - cargo test: `running N tests` sets total; each `test <name> ... ok|FAILED|ignored`
//!   increments current; `test result:` lines finish it.
//! - cargo build: `Compiling <crate>` lines → indeterminate with message `Compiling <crate>`;
//!   `warning:` / `error[` / `error:` count warnings and errors.
//! - jest/vitest: `Tests: 3 failed, 200 passed, 203 total` → current=203 done, total=203.
//! - pytest: `collected N items`, `[ 45%]` percent.
//! - generic: a trailing `N/M` with M ≥ N, or `NN%` at end of line.
//! - `error`/`warning` counters for `npm run lint`-style output (`✖ 3 problems (3 errors, 0 warnings)`).

use codeapp_types::TaskProgress;

#[derive(Default)]
pub struct ProgressParser {
    _private: (),
}

impl ProgressParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one output line. Returns a new progress when it changed.
    pub fn feed_line(&mut self, line: &str, now_ms: u64) -> Option<TaskProgress> {
        let _ = (line, now_ms);
        todo!("ProgressParser::feed_line")
    }

    pub fn warnings(&self) -> u32 {
        todo!("ProgressParser::warnings")
    }

    pub fn errors(&self) -> u32 {
        todo!("ProgressParser::errors")
    }

    pub fn last(&self) -> Option<&TaskProgress> {
        todo!("ProgressParser::last")
    }
}
