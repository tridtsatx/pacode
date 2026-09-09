//! Permission matrix (spec §6.3) and per-session state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use codeapp_tools::ToolKind;
use codeapp_types::{Mode, PermissionDecision, PermissionId, PermissionRequest, RiskLevel};
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
    let _ = (mode, kind, risk, allow_catastrophic);
    todo!("permissions::gate")
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
