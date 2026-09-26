//! Permission matrix (spec §6.3) and per-session state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use pacode_tools::ToolKind;
use pacode_types::{AgentId, Mode, PermissionDecision, PermissionId, PermissionRequest, RiskLevel};
use tokio::sync::oneshot;

use crate::session::Session;

/// Refusal prefix carried by every ZDR denial. Clients and the transcript
/// read it as a policy refusal, not as the user's "no".
pub const ZDR_BLOCKED: &str = "zdr_blocked";

/// ZDR route policy (spec §3: "providers still reachable only for the chosen
/// ZDR model's chat endpoint"). While retention is active, a route the user has
/// not flagged `zdr_capable` must not carry conversation content — including
/// the auxiliary provider calls (compaction summary, permission explain, MCP
/// sampling) that no client ever sees in a permission prompt.
///
/// Checked at the point of the provider call rather than at route selection:
/// the flag can be toggled between the two, and only the later check is the
/// one that keeps data inside the boundary. Returns the refusal reason.
pub fn zdr_route_violation(session: &Session, route: &str) -> Option<String> {
    if !session.zdr_active() || session.model_flags(route).zdr_capable {
        return None;
    }
    Some(format!(
        "{ZDR_BLOCKED}: {route} is not flagged zdr_capable; \
         mark it in the model picker or disable ZDR"
    ))
}

/// ZDR tool policy: network tools and MCP tools (`server__tool`, `__resource`)
/// never run under retention. `run_single_tool` denies them before dispatch;
/// this is the same rule for the permission path, so a tool cannot route around
/// it by asking for permission instead of being gated.
pub fn zdr_tool_violation(session: &Session, tool_name: &str, kind: ToolKind) -> Option<String> {
    if !session.zdr_active() {
        return None;
    }
    if kind == ToolKind::Network || tool_name.contains("__") {
        return Some(format!(
            "{ZDR_BLOCKED}: {tool_name} is disabled while zero-data-retention is active"
        ));
    }
    None
}

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
/// `*` allowed only in Bypass mode when `allow_catastrophic` is set.
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
                    if mode == Mode::Bypass && allow_catastrophic {
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

        for mode in [Mode::Build, Mode::Auto, Mode::Plan] {
            assert!(matches!(
                gate(mode, ToolKind::Exec, Some(RiskLevel::Catastrophic), true),
                GateDecision::Deny(_)
            ));
        }
        assert_eq!(
            gate(
                Mode::Bypass,
                ToolKind::Exec,
                Some(RiskLevel::Catastrophic),
                true
            ),
            GateDecision::Allow
        );
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
        let rx = state.register(req).expect("permission should register");

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
        let _rx = state.register(req).expect("permission should register");

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

        let rx_main = state
            .register(perm_req(&main, "bash"))
            .expect("permission should register");
        let rx_main2 = state
            .register(perm_req(&main, "write"))
            .expect("permission should register");
        let _rx_sub = state
            .register(perm_req(&sub, "bash"))
            .expect("permission should register");

        let cancelled = state.cancel_for_agent(&main);
        assert_eq!(cancelled.len(), 2);
        assert!(state.pending().len() == 1, "subagent's request stays");
        assert_eq!(state.pending()[0].agent, sub);

        let (decision, reason) = rx_main.await.expect("cancelled permission resolves");
        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(reason.as_deref(), Some(CANCELLED_BY_AGENT_REASON));
        let (decision, reason) = rx_main2.await.expect("cancelled permission resolves");
        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(reason.as_deref(), Some(CANCELLED_BY_AGENT_REASON));
    }

    #[test]
    fn the_pending_cap_refuses_rather_than_growing_without_bound() {
        let state = PermissionState::default();
        let mut receivers = Vec::new();
        for index in 0..MAX_PENDING {
            receivers.push(
                state
                    .register(perm_req(&AgentId::main(), &format!("tool_{index}")))
                    .expect("permission should register"),
            );
        }
        assert!(
            state
                .register(perm_req(&AgentId::main(), "tool_overflow"))
                .is_none()
        );
        assert_eq!(state.pending().len(), MAX_PENDING);
        drop(receivers);
    }

    #[tokio::test]
    async fn a_granted_target_short_circuits_the_queue() {
        let state = PermissionState::default();
        assert!(
            matches!(
                state.register_or_cached(perm_req(&AgentId::main(), "bash"), "bash:ls"),
                Registration::Pending(_)
            ),
            "an ungranted target is queued"
        );
        state.allow_for_session("bash:ls".to_string());

        // Same key: no second prompt, and nothing new lands on the queue.
        assert!(matches!(
            state.register_or_cached(perm_req(&AgentId::main(), "bash"), "bash:ls"),
            Registration::Granted
        ));
        assert_eq!(state.pending().len(), 1, "only the first request is queued");

        // The grant is answered from the cache even with the queue at its cap,
        // so a full queue can never push a granted tool back to a prompt.
        while state.pending().len() < MAX_PENDING {
            state.register_or_cached(perm_req(&AgentId::main(), "bash"), "bash:fill");
        }
        assert!(matches!(
            state.register_or_cached(perm_req(&AgentId::main(), "bash"), "bash:fill"),
            Registration::QueueFull
        ));
        assert!(matches!(
            state.register_or_cached(perm_req(&AgentId::main(), "bash"), "bash:ls"),
            Registration::Granted
        ));
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

pub(crate) const CANCELLED_BY_AGENT_REASON: &str = "\0pacode-permission-cancelled";

/// Prompts that may wait for the user at once, per session. Deliberately small:
/// these queue behind a human reading them, so raising it buys no throughput and
/// only makes the client's prompt list unusable. A subagent whose prompt is
/// refused is told so — it is not a throttle on how many agents may work.
pub const MAX_PENDING: usize = 8;

/// Both permission tables live under one mutex: "is this target already granted
/// for the session?" and "put this request on the queue" are a single
/// decision. With two locks a pair of concurrent calls for the same target can
/// both miss the cache and both raise a prompt for one grant.
#[derive(Default)]
struct PermissionStore {
    allowed_for_session: HashSet<String>,
    pending: HashMap<PermissionId, (PermissionRequest, oneshot::Sender<PermissionResolution>)>,
}

/// Outcome of the atomic cache-check + queue-registration step.
pub enum Registration {
    /// The key is already granted for the session: no prompt.
    Granted,
    /// Registered; the receiver resolves to the user's decision.
    Pending(oneshot::Receiver<PermissionResolution>),
    /// `MAX_PENDING` requests are already waiting.
    QueueFull,
}

/// Queue `req` unless the cap is reached. The caller holds the store lock, so
/// the cap check and the insert cannot interleave with another registration.
fn insert_pending(
    store: &mut PermissionStore,
    req: PermissionRequest,
) -> Option<oneshot::Receiver<PermissionResolution>> {
    if store.pending.len() >= MAX_PENDING {
        log::warn!(
            "refusing permission {}: {MAX_PENDING} are already waiting",
            req.id
        );
        return None;
    }
    let (tx, rx) = oneshot::channel();
    log::info!(
        "permission requested: id={} tool={} title={:?}",
        req.id,
        req.tool,
        req.title
    );
    store.pending.insert(req.id.clone(), (req, tx));
    Some(rx)
}

#[derive(Default)]
pub struct PermissionState {
    store: Mutex<PermissionStore>,
}

impl PermissionState {
    fn lock(&self) -> std::sync::MutexGuard<'_, PermissionStore> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_allowed_for_session(&self, key: &str) -> bool {
        self.lock().allowed_for_session.contains(key)
    }

    pub fn allow_for_session(&self, key: String) {
        self.lock().allowed_for_session.insert(key);
    }

    /// Check the AllowSession cache and, on a miss, register the pending
    /// request — one atomic step, see [`PermissionStore`].
    pub fn register_or_cached(&self, req: PermissionRequest, key: &str) -> Registration {
        let mut store = self.lock();
        if store.allowed_for_session.contains(key) {
            return Registration::Granted;
        }
        match insert_pending(&mut store, req) {
            Some(rx) => Registration::Pending(rx),
            None => Registration::QueueFull,
        }
    }

    /// Register a pending request; `None` is returned when the cap is reached.
    pub fn register(
        &self,
        req: PermissionRequest,
    ) -> Option<oneshot::Receiver<PermissionResolution>> {
        insert_pending(&mut self.lock(), req)
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
        match self.lock().pending.remove(id) {
            Some((_, tx)) => tx.send((decision, reason)).is_ok(),
            None => {
                log::warn!("permission decision for unknown id={id}");
                false
            }
        }
    }

    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.lock()
            .pending
            .values()
            .map(|(req, _)| req.clone())
            .collect()
    }

    /// A clone of the pending request for `id` — `None` once resolved or
    /// cancelled (the entry is removed in both cases).
    pub fn get(&self, id: &PermissionId) -> Option<PermissionRequest> {
        self.lock().pending.get(id).map(|(req, _)| req.clone())
    }

    /// Drop a request without answering (agent stopped): the waiting tool sees Deny.
    pub fn cancel(&self, id: &PermissionId) -> bool {
        self.lock().pending.remove(id).is_some()
    }

    pub(crate) fn reassign_agent(&self, from: &AgentId, to: &AgentId) {
        let mut store = self.lock();
        for (request, _) in store.pending.values_mut() {
            if &request.agent == from {
                request.agent = to.clone();
            }
        }
    }

    /// Drop every request `agent` is still waiting on (interrupt/stop_agent).
    /// The requests are returned so the session can emit the resolution event.
    pub fn cancel_for_agent(&self, agent: &AgentId) -> Vec<PermissionRequest> {
        let mut store = self.lock();
        let ids: Vec<PermissionId> = store
            .pending
            .iter()
            .filter(|(_, (req, _))| &req.agent == agent)
            .map(|(id, _)| id.clone())
            .collect();
        let mut cancelled = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some((req, tx)) = store.pending.remove(&id) {
                let _ = tx.send((
                    PermissionDecision::Deny,
                    Some(CANCELLED_BY_AGENT_REASON.to_string()),
                ));
                cancelled.push(req);
            }
        }
        cancelled
    }
}
