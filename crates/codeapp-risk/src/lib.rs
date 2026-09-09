//! Deterministic risk classification for shell commands. Port of jcode's
//! `jcode-command-risk` (see `jcode/crates/jcode-command-risk/src/`): classify by
//! blast radius, bias toward recall, never touch the network.
//!
//! Tiers (`codeapp_types::RiskLevel`):
//! - `Safe`: no destructive potential detected;
//! - `Low`: destructive but bounded (inside cwd, git-recoverable, temp dirs);
//! - `Confirm`: destructive target cannot be determined statically;
//! - `Catastrophic`: would destroy home, root, credentials; never runs.

pub mod gate;
pub mod paths;
pub mod readonly;
pub mod tokenize;

use std::path::Path;

pub use codeapp_types::RiskLevel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskFinding {
    pub level: RiskLevel,
    /// Human-readable reason, shown to the model and in the permission prompt.
    pub reason: String,
    /// The path or argument that triggered it.
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub findings: Vec<RiskFinding>,
}

impl RiskAssessment {
    pub fn safe() -> Self {
        Self {
            level: RiskLevel::Safe,
            findings: Vec::new(),
        }
    }

    /// One-line summary for prompts: the highest finding's reason.
    pub fn summary(&self) -> Option<&str> {
        self.findings
            .iter()
            .max_by_key(|f| f.level)
            .map(|f| f.reason.as_str())
    }
}

/// Classify `command` (a shell string) executed with `cwd`.
pub fn classify(command: &str, cwd: &Path) -> RiskAssessment {
    gate::classify(command, cwd)
}

/// True when every simple command in the pipeline is on the read-only allowlist and
/// there are no redirections to files. Used by Plan mode.
pub fn is_read_only(command: &str) -> bool {
    readonly::is_read_only(command)
}
