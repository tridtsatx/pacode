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

use std::sync::OnceLock;

use codeapp_types::{ProgressSource, TaskProgress};
use regex::Regex;

static ANSI_RE: OnceLock<Regex> = OnceLock::new();
static CARGO_TEST_RUNNING_RE: OnceLock<Regex> = OnceLock::new();
static CARGO_TEST_CASE_RE: OnceLock<Regex> = OnceLock::new();
static CARGO_TEST_RESULT_RE: OnceLock<Regex> = OnceLock::new();
static CARGO_COMPILING_RE: OnceLock<Regex> = OnceLock::new();
static JEST_TESTS_RE: OnceLock<Regex> = OnceLock::new();
static PYTEST_COLLECTED_RE: OnceLock<Regex> = OnceLock::new();
static PYTEST_PERCENT_RE: OnceLock<Regex> = OnceLock::new();
static GENERIC_RATIO_RE: OnceLock<Regex> = OnceLock::new();
static GENERIC_PERCENT_RE: OnceLock<Regex> = OnceLock::new();
static LINT_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();
static LINT_ERRORS_RE: OnceLock<Regex> = OnceLock::new();
static LINT_WARNINGS_RE: OnceLock<Regex> = OnceLock::new();
static WARNING_RE: OnceLock<Regex> = OnceLock::new();
static ERROR_RE: OnceLock<Regex> = OnceLock::new();

fn regex_or_init(lock: &'static OnceLock<Regex>, pattern: &'static str) -> &'static Regex {
    lock.get_or_init(|| match Regex::new(pattern) {
        Ok(re) => re,
        Err(err) => panic!("static regex compilation failed for '{pattern}': {err}"),
    })
}

fn strip_ansi(line: &str) -> String {
    regex_or_init(&ANSI_RE, r"\x1b\[[0-9;]*[a-zA-Z]")
        .replace_all(line, "")
        .into_owned()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressParser {
    cargo_test_total: Option<u64>,
    cargo_test_current: u64,
    pytest_total: Option<u64>,
    warnings: u32,
    errors: u32,
    last: Option<TaskProgress>,
}

impl ProgressParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn parse_warnings_errors(&mut self, line: &str) {
        let is_lint = regex_or_init(&LINT_SUMMARY_RE, r"(?:problem|problems|✖)").is_match(line);
        if is_lint {
            let errs = regex_or_init(&LINT_ERRORS_RE, r"(\d+)\s+errors?")
                .captures(line)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u32>().ok());
            let warns = regex_or_init(&LINT_WARNINGS_RE, r"(\d+)\s+warnings?")
                .captures(line)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u32>().ok());
            if let Some(e) = errs {
                self.errors = e;
            }
            if let Some(w) = warns {
                self.warnings = w;
            }
        } else {
            if regex_or_init(&WARNING_RE, r"\bwarning:|\bwarning\[").is_match(line) {
                self.warnings = self.warnings.saturating_add(1);
            }
            if regex_or_init(&ERROR_RE, r"\berror:|\berror\[").is_match(line) {
                self.errors = self.errors.saturating_add(1);
            }
        }
    }

    fn parse_progress(&mut self, line: &str, now_ms: u64) -> Option<TaskProgress> {
        // 1. Cargo test
        if let Some(caps) =
            regex_or_init(&CARGO_TEST_RUNNING_RE, r"^running (\d+) tests?").captures(line)
        {
            let total = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok())?;
            self.cargo_test_total = Some(total);
            self.cargo_test_current = 0;
            return Some(TaskProgress {
                current: Some(0),
                total: Some(total),
                percent: Some(0.0),
                message: None,
                source: ProgressSource::Parsed,
                updated_at_ms: now_ms,
            });
        }

        if let Some(caps) = regex_or_init(
            &CARGO_TEST_CASE_RE,
            r"^test (.+?) \.\.\. (?:ok|FAILED|ignored)",
        )
        .captures(line)
        {
            let test_name = caps
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            self.cargo_test_current = self.cargo_test_current.saturating_add(1);
            return Some(
                TaskProgress {
                    current: Some(self.cargo_test_current),
                    total: self.cargo_test_total,
                    percent: None,
                    message: Some(test_name),
                    source: ProgressSource::Parsed,
                    updated_at_ms: now_ms,
                }
                .normalize(),
            );
        }

        if regex_or_init(&CARGO_TEST_RESULT_RE, r"^test result:").is_match(line)
            && let Some(total) = self.cargo_test_total
        {
            return Some(TaskProgress {
                current: Some(total),
                total: Some(total),
                percent: Some(100.0),
                message: None,
                source: ProgressSource::Parsed,
                updated_at_ms: now_ms,
            });
        }

        // 2. Cargo build
        if let Some(caps) =
            regex_or_init(&CARGO_COMPILING_RE, r"^\s*Compiling\s+([a-zA-Z0-9_\-]+)").captures(line)
        {
            let crate_name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            return Some(TaskProgress {
                current: None,
                total: None,
                percent: None,
                message: Some(format!("Compiling {crate_name}")),
                source: ProgressSource::Parsed,
                updated_at_ms: now_ms,
            });
        }

        // 3. Jest / Vitest
        if let Some(caps) =
            regex_or_init(&JEST_TESTS_RE, r"^\s*Tests:\s+.*?\b(\d+)\s+total").captures(line)
        {
            let total = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok())?;
            return Some(TaskProgress {
                current: Some(total),
                total: Some(total),
                percent: Some(100.0),
                message: None,
                source: ProgressSource::Parsed,
                updated_at_ms: now_ms,
            });
        }

        // 4. Pytest
        if let Some(caps) =
            regex_or_init(&PYTEST_COLLECTED_RE, r"(?:^|\s)collected (\d+) items?").captures(line)
        {
            let total = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok())?;
            self.pytest_total = Some(total);
            return Some(TaskProgress {
                current: Some(0),
                total: Some(total),
                percent: Some(0.0),
                message: None,
                source: ProgressSource::Parsed,
                updated_at_ms: now_ms,
            });
        }

        if let Some(caps) = regex_or_init(&PYTEST_PERCENT_RE, r"\[\s*(\d+)%\]").captures(line) {
            let pct = caps.get(1).and_then(|m| m.as_str().parse::<f32>().ok())?;
            return Some(
                TaskProgress {
                    current: None,
                    total: self.pytest_total,
                    percent: Some(pct),
                    message: None,
                    source: ProgressSource::Parsed,
                    updated_at_ms: now_ms,
                }
                .normalize(),
            );
        }

        // 5. Generic trailing N/M
        if let Some(caps) = regex_or_init(
            &GENERIC_RATIO_RE,
            r"(?:\s|^)(?P<cur>\d+)\s*/\s*(?P<tot>\d+)\s*$",
        )
        .captures(line)
        {
            let cur = caps
                .name("cur")
                .and_then(|m| m.as_str().parse::<u64>().ok())?;
            let tot = caps
                .name("tot")
                .and_then(|m| m.as_str().parse::<u64>().ok())?;
            if tot > 0 && cur <= tot {
                return Some(
                    TaskProgress {
                        current: Some(cur),
                        total: Some(tot),
                        percent: None,
                        message: None,
                        source: ProgressSource::Parsed,
                        updated_at_ms: now_ms,
                    }
                    .normalize(),
                );
            }
        }

        // 6. Generic trailing NN%
        if let Some(caps) =
            regex_or_init(&GENERIC_PERCENT_RE, r"(?:\s|^)(?P<pct>100|[1-9]?\d)%\s*$").captures(line)
        {
            let pct = caps
                .name("pct")
                .and_then(|m| m.as_str().parse::<f32>().ok())?;
            return Some(
                TaskProgress {
                    current: None,
                    total: None,
                    percent: Some(pct),
                    message: None,
                    source: ProgressSource::Parsed,
                    updated_at_ms: now_ms,
                }
                .normalize(),
            );
        }

        None
    }

    /// Feed one output line. Returns a new progress when it changed.
    pub fn feed_line(&mut self, line: &str, now_ms: u64) -> Option<TaskProgress> {
        let stripped = strip_ansi(line);
        let trimmed = stripped.trim();

        self.parse_warnings_errors(trimmed);

        if let Some(candidate) = self.parse_progress(trimmed, now_ms) {
            let changed = match &self.last {
                None => true,
                Some(last) => {
                    last.current != candidate.current
                        || last.total != candidate.total
                        || last.percent != candidate.percent
                        || last.message != candidate.message
                }
            };

            if changed {
                self.last = Some(candidate.clone());
                return Some(candidate);
            }
        }

        None
    }

    pub fn warnings(&self) -> u32 {
        self.warnings
    }

    pub fn errors(&self) -> u32 {
        self.errors
    }

    pub fn last(&self) -> Option<&TaskProgress> {
        self.last.as_ref()
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod progress_tests;
