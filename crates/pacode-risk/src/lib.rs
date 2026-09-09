//! Deterministic risk classification for shell commands. Port of jcode's
//! `jcode-command-risk` (see `jcode/crates/jcode-command-risk/src/`): classify by
//! blast radius, bias toward recall, never touch the network.
//!
//! Tiers (`pacode_types::RiskLevel`):
//! - `Safe`: no destructive potential detected;
//! - `Low`: destructive but bounded (inside cwd, git-recoverable, temp dirs);
//! - `Confirm`: destructive target cannot be determined statically;
//! - `Catastrophic`: would destroy home, root, credentials; never runs.

pub mod gate;
pub mod paths;
pub mod readonly;
pub mod tokenize;

use std::path::Path;

pub use pacode_types::RiskLevel;

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

    pub fn from_findings(findings: Vec<RiskFinding>) -> Self {
        let level = findings
            .iter()
            .map(|f| f.level)
            .max()
            .unwrap_or(RiskLevel::Safe);
        Self { level, findings }
    }

    /// One-line summary for prompts: the highest finding's reason.
    pub fn summary(&self) -> Option<&str> {
        self.findings
            .iter()
            .max_by_key(|f| f.level)
            .map(|f| f.reason.as_str())
    }

    /// Multi-line explanation of all findings.
    pub fn explanation(&self) -> String {
        let mut out = String::new();
        for finding in &self.findings {
            out.push_str("- ");
            out.push_str(&finding.reason);
            if let Some(target) = &finding.target {
                out.push_str(&format!(" (target: {target})"));
            }
            out.push('\n');
        }
        out
    }

    /// Whether this command may proceed without a reflection/confirmation prompt.
    pub fn runs_immediately(&self) -> bool {
        matches!(self.level, RiskLevel::Safe | RiskLevel::Low)
    }

    /// Whether any confirmation could ever unlock this.
    pub fn is_absolute_deny(&self) -> bool {
        matches!(self.level, RiskLevel::Catastrophic)
    }
}

/// Classify `command` (a shell string) executed with `cwd`.
pub fn classify(command: &str, cwd: &Path) -> RiskAssessment {
    gate::classify(command, cwd)
}

/// Classify `command` with an explicit `cwd` and `home` directory.
pub fn classify_with_home(command: &str, cwd: &Path, home: &Path) -> RiskAssessment {
    gate::classify_with_home(command, cwd, home)
}

/// True when every simple command in the pipeline is on the read-only allowlist and
/// there are no redirections to files. Used by Plan mode.
pub fn is_read_only(command: &str) -> bool {
    readonly::is_read_only(command)
}
