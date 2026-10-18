//! Rule model: what a parsed rule file looks like and how it is classified.

use std::path::PathBuf;

use regex::Regex;

/// How loudly a finding is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Appended to the tool result; the call still counts as successful.
    Warn,
    /// Appended to the tool result AND the result is marked as an error.
    Block,
}

/// When a rule is evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Only after an edit/write tool call succeeded (default).
    AfterWrite,
    /// While the tool arguments are still streaming (cuts the call short), and
    /// again after the write as a fallback when the retry budget is spent.
    Stream,
}

/// Which part of a line the `match` regex sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Comments and string/char literals are blanked out first (best effort).
    Code,
    /// The raw line.
    Any,
}

/// Where a rule came from; later layers override earlier ones by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Builtin(&'static str),
    Global(PathBuf),
    Project(PathBuf),
}

impl Origin {
    pub fn label(&self) -> String {
        match self {
            Origin::Builtin(file) => format!("builtin:{file}"),
            Origin::Global(p) | Origin::Project(p) => p.display().to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    /// Unique id, `[a-z0-9][a-z0-9_.-]*`.
    pub name: String,
    /// Lowercase file extensions without the dot (`rs`, `ts`, `tsx`).
    pub languages: Vec<String>,
    /// Matched per ADDED line.
    pub pattern: Regex,
    /// Suppresses a hit when it matches the raw line from the hit start onward.
    pub unless: Option<Regex>,
    /// Suppresses a hit when it matches inside the contiguous comment/attribute
    /// block directly above the line (e.g. `// SAFETY:`).
    pub unless_above: Option<Regex>,
    pub scope: Scope,
    pub severity: Severity,
    pub mode: Mode,
    /// Max times this rule may fire per turn (a firing = one tool call with a hit).
    pub max_per_turn: u32,
    /// Path patterns the rule never applies to (see [`crate::path_excluded`]).
    pub exclude: Vec<String>,
    /// Rust only: skip `#[cfg(test)]` / `#[test]` items.
    pub skip_test_code: bool,
    /// The text shown to the model (markdown body of the rule file).
    pub message: String,
    pub origin: Origin,
}

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("{origin}: missing or unterminated `---` frontmatter")]
    Frontmatter { origin: String },
    #[error("{origin}: missing field `{field}`")]
    MissingField { origin: String, field: &'static str },
    #[error("{origin}: invalid value for `{field}`: {value}")]
    BadValue {
        origin: String,
        field: &'static str,
        value: String,
    },
    #[error("{origin}: invalid regex in `{field}`: {source}")]
    BadRegex {
        origin: String,
        field: &'static str,
        source: regex::Error,
    },
    #[error("{origin}: {message}")]
    Io { origin: String, message: String },
}
