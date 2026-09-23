//! Permission matrix (spec §6.3) and per-session state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use pacode_tools::ToolKind;
use pacode_types::{AgentId, Mode, PermissionDecision, PermissionId, PermissionRequest, RiskLevel};
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
    let decision = match kind {
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
    };
    log::debug!("permission gate: mode={mode:?} kind={kind:?} risk={risk:?} decision={decision:?}");
    decision
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

    fn perm_req(agent: &AgentId, tool: &str) -> PermissionRequest {
        PermissionRequest {
            id: PermissionId::generate(),
            agent: agent.clone(),
            agent_name: agent.to_string(),
            call_id: pacode_types::CallId::new("call_test"),
            tool: tool.to_string(),
            title: format!("{tool} title"),
            detail: String::new(),
            risk: None,
            created_at_ms: pacode_types::now_ms(),
        }
    }

    #[tokio::test]
    async fn resolve_delivers_decision_and_reason() {
        let state = PermissionState::default();
        let req = perm_req(&AgentId::main(), "bash");
        let id = req.id.clone();
        let rx = state.register(req);

        assert!(state.resolve(&id, PermissionDecision::Deny, Some("no".to_string())));
        let (decision, reason) = rx.await.unwrap();
        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(reason.as_deref(), Some("no"));

        // A resolved id is gone: a second resolve reports unknown.
        assert!(!state.resolve(&id, PermissionDecision::AllowOnce, None));
    }

    #[tokio::test]
    async fn get_returns_pending_request_until_resolved() {
        let state = PermissionState::default();
        let req = perm_req(&AgentId::main(), "write");
        let id = req.id.clone();
        let _rx = state.register(req);

        let fetched = state.get(&id).expect("pending request must be fetchable");
        assert_eq!(fetched.id, id);
        assert_eq!(fetched.tool, "write");

        // Resolved is gone — an explain request against it must fail.
        assert!(state.resolve(&id, PermissionDecision::AllowOnce, None));
        assert!(state.get(&id).is_none());
        assert!(state.get(&PermissionId::new("perm_nope")).is_none());
    }

    #[tokio::test]
    async fn cancel_for_agent_removes_only_that_agents_requests() {
        let state = PermissionState::default();
        let main = AgentId::main();
        let sub = AgentId::new("sub_1");

        let rx_main = state.register(perm_req(&main, "bash"));
        let rx_main2 = state.register(perm_req(&main, "write"));
        let _rx_sub = state.register(perm_req(&sub, "bash"));

        let cancelled = state.cancel_for_agent(&main);
        assert_eq!(cancelled.len(), 2);
        assert!(state.pending().len() == 1, "subagent's request stays");
        assert_eq!(state.pending()[0].agent, sub);

        // Dropped senders resolve the waiters to Deny (RecvError on our side).
        assert!(rx_main.await.is_err());
        assert!(rx_main2.await.is_err());
    }
}

/// Key for the AllowSession cache: `"<tool>:<target>"` where target is the file path
/// for edits or the normalized command for exec.
pub fn session_key(tool: &str, target: &str) -> String {
    format!("{tool}:{}", target.trim())
}

/// What a pending request resolves to: the decision plus, for `Deny`, an
/// optional explanation that was not the user's plain refusal (policy reason,
/// `interrupted`, `zdr_blocked: …`, a client-supplied note).
pub type PermissionResolution = (PermissionDecision, Option<String>);

#[derive(Default)]
pub struct PermissionState {
    allowed_for_session: Mutex<HashSet<String>>,
    pending:
        Mutex<HashMap<PermissionId, (PermissionRequest, oneshot::Sender<PermissionResolution>)>>,
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
    pub fn register(&self, req: PermissionRequest) -> oneshot::Receiver<PermissionResolution> {
        log::info!(
            "permission requested: id={} tool={} title={:?}",
            req.id,
            req.tool,
            req.title
        );
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(req.id.clone(), (req, tx));
        }
        rx
    }

    /// Resolve a pending request. Returns false when unknown. `reason` rides
    /// along to the waiting call, the wire event and the transcript.
    pub fn resolve(
        &self,
        id: &PermissionId,
        decision: PermissionDecision,
        reason: Option<String>,
    ) -> bool {
        log::info!("permission decision: id={id} decision={decision:?} reason={reason:?}");
        let entry = self.pending.lock().ok().and_then(|mut p| p.remove(id));
        match entry {
            Some((_, tx)) => tx.send((decision, reason)).is_ok(),
            None => {
                log::warn!("permission decision for unknown id={id}");
                false
            }
        }
    }

    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(req, _)| req.clone()).collect())
            .unwrap_or_default()
    }

    /// A clone of the pending request for `id` — `None` once resolved or
    /// cancelled (the entry is removed in both cases).
    pub fn get(&self, id: &PermissionId) -> Option<PermissionRequest> {
        self.pending
            .lock()
            .ok()?
            .get(id)
            .map(|(req, _)| req.clone())
    }

    /// Drop a request without answering (agent stopped): the waiting tool sees Deny.
    pub fn cancel(&self, id: &PermissionId) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(id);
        }
    }

    /// Drop every request `agent` is still waiting on (interrupt/stop_agent).
    /// Removing an entry drops its sender, so the waiting call resolves to
    /// `Deny`; the requests are returned so the session can emit
    /// `PermissionResolved` + the transcript notice for each.
    pub fn cancel_for_agent(&self, agent: &AgentId) -> Vec<PermissionRequest> {
        let mut pending = match self.pending.lock() {
            Ok(p) => p,
            Err(p) => p.into_inner(),
        };
        let ids: Vec<PermissionId> = pending
            .iter()
            .filter(|(_, (req, _))| &req.agent == agent)
            .map(|(id, _)| id.clone())
            .collect();
        let mut cancelled = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some((req, _tx)) = pending.remove(&id) {
                cancelled.push(req);
            }
        }
        cancelled
    }
}
