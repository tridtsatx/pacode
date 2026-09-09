//! Permission matrix (spec §6.3) and per-session state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use pacode_tools::ToolKind;
use pacode_types::{Mode, PermissionDecision, PermissionId, PermissionRequest, RiskLevel};
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Ask,
    Deny(String),
}

/// The matrix. `risk` is `Some` only for `ToolKind::Exec`.
///
/// | action                    | Build | Auto | Plan | Bypass |
/// |---------------------------|-------|------|------|--------|
/// | ReadOnly / Control        | allow | allow| allow| allow  |
/// | Edit                      | ask   | allow| deny | allow  |
/// | Network                   | allow | allow| allow| allow  |
/// | Exec Safe                 | allow | allow| allow| allow  |
/// | Exec Low                  | ask   | allow| deny | allow  |
/// | Exec Confirm              | ask   | ask  | deny | allow  |
/// | Exec Catastrophic         | deny  | deny | deny | deny*  |
///
/// `*` allowed only when `allow_catastrophic` is set.
pub fn gate(
    mode: Mode,
    kind: ToolKind,
    risk: Option<RiskLevel>,
    allow_catastrophic: bool,
) -> GateDecision {
    match kind {
        ToolKind::ReadOnly | ToolKind::Control | ToolKind::Network => GateDecision::Allow,
        ToolKind::Edit => match mode {
            Mode::Build => GateDecision::Ask,
            Mode::Auto => GateDecision::Allow,
            Mode::Plan => {
                GateDecision::Deny("file edits are not permitted in plan mode".to_string())
            }
            Mode::Bypass => GateDecision::Allow,
        },
        ToolKind::Exec => {
            let exec_risk = risk.unwrap_or(RiskLevel::Confirm);
            match exec_risk {
                RiskLevel::Catastrophic => {
                    if allow_catastrophic {
                        GateDecision::Allow
                    } else {
                        GateDecision::Deny(
                            "catastrophic operations are denied by policy".to_string(),
                        )
                    }
                }
                RiskLevel::Safe => GateDecision::Allow,
                RiskLevel::Low => match mode {
                    Mode::Build => GateDecision::Ask,
                    Mode::Auto => GateDecision::Allow,
                    Mode::Plan => GateDecision::Deny(
                        "exec operations are not permitted in plan mode".to_string(),
                    ),
                    Mode::Bypass => GateDecision::Allow,
                },
                RiskLevel::Confirm => match mode {
                    Mode::Build => GateDecision::Ask,
                    Mode::Auto => GateDecision::Ask,
                    Mode::Plan => GateDecision::Deny(
                        "exec operations are not permitted in plan mode".to_string(),
                    ),
                    Mode::Bypass => GateDecision::Allow,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_types::RiskLevel;

    #[test]
    fn test_gate_table_all_modes() {
        // ReadOnly & Network: Allow in all modes
        for mode in [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass] {
            assert_eq!(
                gate(mode, ToolKind::ReadOnly, None, false),
                GateDecision::Allow
            );
            assert_eq!(
                gate(mode, ToolKind::Network, None, false),
                GateDecision::Allow
            );
        }

        // Edit
        assert_eq!(
            gate(Mode::Build, ToolKind::Edit, None, false),
            GateDecision::Ask
        );
        assert_eq!(
            gate(Mode::Auto, ToolKind::Edit, None, false),
            GateDecision::Allow
        );
        assert!(matches!(
            gate(Mode::Plan, ToolKind::Edit, None, false),
            GateDecision::Deny(_)
        ));
        assert_eq!(
            gate(Mode::Bypass, ToolKind::Edit, None, false),
            GateDecision::Allow
        );

        // Exec Safe
        for mode in [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass] {
            assert_eq!(
                gate(mode, ToolKind::Exec, Some(RiskLevel::Safe), false),
                GateDecision::Allow
            );
        }

        // Exec Low
        assert_eq!(
            gate(Mode::Build, ToolKind::Exec, Some(RiskLevel::Low), false),
            GateDecision::Ask
        );
        assert_eq!(
            gate(Mode::Auto, ToolKind::Exec, Some(RiskLevel::Low), false),
            GateDecision::Allow
        );
        assert!(matches!(
            gate(Mode::Plan, ToolKind::Exec, Some(RiskLevel::Low), false),
            GateDecision::Deny(_)
        ));
        assert_eq!(
            gate(Mode::Bypass, ToolKind::Exec, Some(RiskLevel::Low), false),
            GateDecision::Allow
        );

        // Exec Confirm
        assert_eq!(
            gate(Mode::Build, ToolKind::Exec, Some(RiskLevel::Confirm), false),
            GateDecision::Ask
        );
        assert_eq!(
            gate(Mode::Auto, ToolKind::Exec, Some(RiskLevel::Confirm), false),
            GateDecision::Ask
        );
        assert!(matches!(
            gate(Mode::Plan, ToolKind::Exec, Some(RiskLevel::Confirm), false),
            GateDecision::Deny(_)
        ));
        assert_eq!(
            gate(
                Mode::Bypass,
                ToolKind::Exec,
                Some(RiskLevel::Confirm),
                false
            ),
            GateDecision::Allow
        );

        // Exec Catastrophic without allow_catastrophic
        for mode in [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass] {
            assert!(matches!(
                gate(mode, ToolKind::Exec, Some(RiskLevel::Catastrophic), false),
                GateDecision::Deny(_)
            ));
        }

        // Exec Catastrophic with allow_catastrophic
        for mode in [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass] {
            assert_eq!(
                gate(mode, ToolKind::Exec, Some(RiskLevel::Catastrophic), true),
                GateDecision::Allow
            );
        }
    }
}

/// Key for the AllowSession cache: `"<tool>:<target>"` where target is the file path
/// for edits or the normalized command for exec.
pub fn session_key(tool: &str, target: &str) -> String {
    format!("{tool}:{}", target.trim())
}

#[derive(Default)]
pub struct PermissionState {
    allowed_for_session: Mutex<HashSet<String>>,
    pending: Mutex<HashMap<PermissionId, (PermissionRequest, oneshot::Sender<PermissionDecision>)>>,
}

impl PermissionState {
    pub fn is_allowed_for_session(&self, key: &str) -> bool {
        self.allowed_for_session
            .lock()
            .map(|s| s.contains(key))
            .unwrap_or(false)
    }

    pub fn allow_for_session(&self, key: String) {
        if let Ok(mut s) = self.allowed_for_session.lock() {
            s.insert(key);
        }
    }

    /// Register a pending request; the returned receiver resolves on `resolve`.
    pub fn register(&self, req: PermissionRequest) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(req.id.clone(), (req, tx));
        }
        rx
    }

    /// Resolve a pending request. Returns false when unknown.
    pub fn resolve(&self, id: &PermissionId, decision: PermissionDecision) -> bool {
        let entry = self.pending.lock().ok().and_then(|mut p| p.remove(id));
        match entry {
            Some((_, tx)) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(req, _)| req.clone()).collect())
            .unwrap_or_default()
    }

    /// Drop a request without answering (agent stopped): the waiting tool sees Deny.
    pub fn cancel(&self, id: &PermissionId) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(id);
        }
    }
}
