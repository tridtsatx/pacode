//! Command risk classification entry point and the reflection gate.
//!
//! Classification evaluates every command a line runs by blast radius (see
//! [`crate::assess`]); the gate turns the resulting tier into allow, reflect,
//! or deny.

use std::path::{Path, PathBuf};

use crate::assess::Assessor;
use crate::{RiskAssessment, RiskLevel};

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;

/// Outcome of the reflection gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    Reflect { prompt: String },
    Deny { reason: String },
}

/// A justification supplied by the model on a reflection turn.
#[derive(Debug, Clone, Default)]
pub struct Justification {
    pub text: Option<String>,
}

impl Justification {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
        }
    }

    pub fn is_substantive(&self) -> bool {
        let Some(text) = self.text.as_deref().map(str::trim) else {
            return false;
        };
        if text.len() < MIN_JUSTIFICATION_LEN {
            return false;
        }
        const EMPTY_AFFIRMATIONS: &[&str] = &[
            "yes",
            "ok",
            "okay",
            "sure",
            "confirmed",
            "proceed",
            "do it",
            "continue",
            "y",
            "approved",
            "go ahead",
        ];
        let lowered = text.to_lowercase();
        let stripped = lowered.trim_end_matches(['.', '!', '?']).trim();
        !EMPTY_AFFIRMATIONS.contains(&stripped)
    }
}

const MIN_JUSTIFICATION_LEN: usize = 25;

/// Decide whether an assessed command runs, reflects, or is permanently denied.
pub fn gate(assessment: &RiskAssessment, justification: &Justification) -> GateOutcome {
    match assessment.level {
        RiskLevel::Safe | RiskLevel::Low => GateOutcome::Allow,
        RiskLevel::Catastrophic => {
            let expl = assessment.explanation();
            GateOutcome::Deny {
                reason: format!(
                    "This command is blocked and cannot be confirmed.\n\n{expl}\n\
                     If the user genuinely wants this, they must run it themselves outside the agent."
                ),
            }
        }
        RiskLevel::Confirm => {
            if justification.is_substantive() {
                GateOutcome::Allow
            } else {
                GateOutcome::Reflect {
                    prompt: reflection_prompt(assessment),
                }
            }
        }
    }
}

fn reflection_prompt(assessment: &RiskAssessment) -> String {
    let expl = assessment.explanation();
    format!(
        "This command was not run. It is irreversible:\n\n{expl}\n\
         Before it can proceed, stop and check it against the user's actual request:\n\
         - Which specific thing the user asked for requires deleting this?\n\
         - Did the user name this path, or did you infer it?\n\
         - If you inferred it, is a narrower target enough?\n\
         - If this is wrong, nothing here can be recovered.\n\n\
         If it is genuinely what the user asked for, re-issue the same call with a \
         `justification` field explaining which request it serves. If you are not sure, \
         ask the user instead: that costs one message, and being wrong costs their data."
    )
}

/// Classify command using `HOME` from the environment.
pub fn classify(command: &str, cwd: &Path) -> RiskAssessment {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    classify_with_home(command, cwd, &home)
}

/// Classify command with an explicit cwd and home directory.
pub fn classify_with_home(command: &str, cwd: &Path, home: &Path) -> RiskAssessment {
    let mut assessor = Assessor::new(cwd, home);
    assessor.script(command, false, 0);
    if assessor.findings.is_empty() {
        RiskAssessment::safe()
    } else {
        RiskAssessment::from_findings(assessor.findings)
    }
}
