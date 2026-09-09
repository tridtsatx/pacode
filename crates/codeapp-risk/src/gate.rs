//! Classification entry point: tokenize, walk simple commands, apply rules.

use std::path::Path;

use crate::RiskAssessment;

pub fn classify(command: &str, cwd: &Path) -> RiskAssessment {
    let _ = (command, cwd);
    todo!("gate::classify")
}
