//! The permission flow of a tool call: the gate, the session grant cache,
//! the prompt, and the wait for the user's answer (or the turn's cancel).

use pacode_tools::host::{PermissionDraft, PermissionOutcome};
use pacode_types::PermissionDecision;

use super::SessionHost;

/// The resource a permission prompt is about, as the `AllowSession` cache keys
/// it: the path for file edits, the detail (the command) for everything else.
pub(crate) fn permission_target(tool: &str, title: &str, detail: &str) -> String {
    if !matches!(tool, "write" | "edit" | "multi_edit") {
        return detail.trim().to_string();
    }
    let title = title.trim();
    let target = title
        .split_once(' ')
        .map(|(_, target)| target.trim())
        .unwrap_or(title)
        .strip_suffix(" (outside workspace)")
        .unwrap_or_else(|| {
            title
                .split_once(' ')
                .map(|(_, target)| target.trim())
                .unwrap_or(title)
        })
        .trim();
    if target.is_empty() {
        detail.trim().to_string()
    } else {
        target.to_string()
    }
}

impl SessionHost {
    pub(super) async fn request_permission_inner(
        &self,
        draft: PermissionDraft,
        force_ask: bool,
    ) -> PermissionOutcome {
        let (tool_name, kind) = {
            let session_tools = self.session.tools.read().unwrap_or_else(|p| p.into_inner());

            let name = if let Some(ref name) = draft.tool_name {
                name.clone()
            } else {
                let prefix = draft
                    .title
                    .split(|c: char| c == ':' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                if session_tools.get(&prefix).is_some() {
                    prefix
                } else if draft.risk.is_some() {
                    "bash".to_string()
                } else {
                    prefix
                }
            };

            let kind = if let Some(k) = draft.tool_kind {
                k
            } else if draft.risk.is_some() {
                pacode_tools::ToolKind::Exec
            } else if let Some(t) = session_tools.get(&name) {
                t.kind()
            } else if name == "bash" {
                pacode_tools::ToolKind::Exec
            } else if name == "write" || name == "edit" || name == "multi_edit" {
                pacode_tools::ToolKind::Edit
            } else {
                // Unknown or unclassifiable tool MUST fail closed:
                // treat as the most restrictive kind so it requires an explicit permission prompt.
                pacode_tools::ToolKind::Exec
            };

            (name, kind)
        };

        // ZDR hard isolation for tools that do ask (network tools normally
        // bypass this path; `run_single_tool` denies them before dispatch).
        if let Some(reason) =
            crate::permissions::zdr_tool_violation(&self.session, &tool_name, kind)
        {
            return PermissionOutcome {
                decision: PermissionDecision::Deny,
                reason: Some(reason),
            };
        }

        let mode = self.session.meta().mode;
        let allow_catastrophic = self.session.config.permissions.allow_catastrophic;
        let gate_decision = if force_ask {
            crate::permissions::GateDecision::Ask
        } else {
            crate::permissions::gate(mode, kind, draft.risk, allow_catastrophic)
        };

        match gate_decision {
            crate::permissions::GateDecision::Allow => PermissionOutcome {
                decision: PermissionDecision::AllowOnce,
                reason: None,
            },
            crate::permissions::GateDecision::Deny(reason) => PermissionOutcome {
                decision: PermissionDecision::Deny,
                reason: Some(reason),
            },
            crate::permissions::GateDecision::Ask => {
                let owner = self.owner_agent();
                let owner_id = owner
                    .as_ref()
                    .map(|agent| agent.id())
                    .unwrap_or_else(|| self.agent.clone());
                let target = permission_target(&tool_name, &draft.title, &draft.detail);
                let cache_key = crate::permissions::session_key(&tool_name, &target);
                let owner_name = owner
                    .as_ref()
                    .map(|agent| {
                        agent
                            .info
                            .read()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .name
                            .clone()
                    })
                    .unwrap_or_else(|| draft.agent_name.clone());

                let perm_id = pacode_types::PermissionId::generate();
                let perm_req = pacode_types::PermissionRequest {
                    id: perm_id.clone(),
                    agent: owner_id.clone(),
                    agent_name: owner_name,
                    call_id: draft.call_id.clone(),
                    tool: tool_name,
                    title: draft.title.clone(),
                    detail: draft.detail.clone(),
                    risk: draft.risk,
                    created_at_ms: pacode_types::now_ms(),
                };

                // One atomic step: the session grant and the queue insert are
                // decided together, so two concurrent calls for the same target
                // cannot both miss the cache and raise two prompts.
                let mut rx = match self
                    .session
                    .permissions
                    .register_or_cached(perm_req.clone(), &cache_key)
                {
                    crate::permissions::Registration::Granted => {
                        return PermissionOutcome {
                            decision: PermissionDecision::AllowSession,
                            reason: None,
                        };
                    }
                    crate::permissions::Registration::QueueFull => {
                        return PermissionOutcome {
                            decision: PermissionDecision::Deny,
                            reason: Some(format!(
                                "permission queue is full (maximum {})",
                                crate::permissions::MAX_PENDING
                            )),
                        };
                    }
                    crate::permissions::Registration::Pending(rx) => rx,
                };
                self.session
                    .events
                    .emit(pacode_types::Event::PermissionRequested(perm_req.clone()));
                self.session.emit_session_status();
                crate::hooks::notification(&self.session, &owner_id, Some(perm_req.tool.clone()));

                let agent_opt = owner;
                let item_seq = agent_opt
                    .as_ref()
                    .map(|a| {
                        a.transcript
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .next_seq()
                    })
                    .unwrap_or(0);

                let perm_item = pacode_types::TranscriptItem {
                    seq: item_seq,
                    agent: owner_id.clone(),
                    ts_ms: perm_req.created_at_ms,
                    kind: pacode_types::TranscriptKind::Permission(perm_req.clone()),
                };

                if let Some(agent) = &agent_opt {
                    agent
                        .transcript
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .upsert(perm_item.clone());
                }
                self.session
                    .events
                    .emit(pacode_types::Event::ItemAdded(perm_item.clone()));

                let (prev_status, prev_activity) = agent_opt
                    .as_ref()
                    .map(|agent| {
                        let info = agent.info.read().unwrap_or_else(|p| p.into_inner());
                        (info.status, info.activity.clone())
                    })
                    .unwrap_or((pacode_types::AgentStatus::Idle, None));

                if let Some(agent) = &agent_opt {
                    // From the status read above, in one step: a stop that
                    // landed since keeps `Stopped`.
                    agent.restore_status(
                        prev_status,
                        pacode_types::AgentStatus::WaitingApproval,
                        Some(draft.title.clone()),
                    );
                    self.session
                        .events
                        .emit(pacode_types::Event::AgentUpdated(agent.info()));
                }

                let cancel_token = agent_opt
                    .as_ref()
                    .and_then(|a| a.cancel.lock().unwrap_or_else(|p| p.into_inner()).clone());
                let normalize = |decision: PermissionDecision, reason: Option<String>| {
                    if reason.as_deref() == Some(crate::permissions::CANCELLED_BY_AGENT_REASON) {
                        (
                            PermissionDecision::Deny,
                            Some("interrupted".to_string()),
                            false,
                        )
                    } else {
                        (decision, reason, true)
                    }
                };

                let (decision, reason, emit_resolution) = match cancel_token {
                    Some(token) => {
                        let mut received = None;
                        let cancelled = {
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => true,
                                res = &mut rx => {
                                    received = Some(res);
                                    false
                                }
                            }
                        };
                        if cancelled {
                            if self.session.permissions.cancel(&perm_id) {
                                (
                                    PermissionDecision::Deny,
                                    Some("interrupted".to_string()),
                                    true,
                                )
                            } else if let Ok((decision, reason)) = rx.try_recv() {
                                normalize(decision, reason)
                            } else {
                                (
                                    PermissionDecision::Deny,
                                    Some("interrupted".to_string()),
                                    false,
                                )
                            }
                        } else {
                            match received {
                                Some(Ok((decision, reason))) => normalize(decision, reason),
                                Some(Err(_)) if token.is_cancelled() => (
                                    PermissionDecision::Deny,
                                    Some("interrupted".to_string()),
                                    false,
                                ),
                                Some(Err(_)) => (
                                    PermissionDecision::Deny,
                                    Some("permission request aborted".to_string()),
                                    true,
                                ),
                                None => (
                                    PermissionDecision::Deny,
                                    Some("permission request aborted".to_string()),
                                    true,
                                ),
                            }
                        }
                    }
                    None => match rx.await {
                        Ok((decision, reason)) => normalize(decision, reason),
                        Err(_) => (
                            PermissionDecision::Deny,
                            Some("permission request aborted".to_string()),
                            true,
                        ),
                    },
                };

                if emit_resolution {
                    self.session.emit_permission_resolution(
                        &perm_req,
                        Some(perm_item.seq),
                        decision,
                        reason.clone(),
                    );
                }

                if decision == PermissionDecision::AllowSession {
                    self.session.permissions.allow_for_session(cache_key);
                }

                if let Some(agent) = &agent_opt {
                    // A stop that landed during the permission wait wins over
                    // restoring the pre-permission status: only a status still
                    // `WaitingApproval` is put back, checked and set under one
                    // lock.
                    agent.restore_status(
                        pacode_types::AgentStatus::WaitingApproval,
                        prev_status,
                        prev_activity,
                    );
                    self.session
                        .events
                        .emit(pacode_types::Event::AgentUpdated(agent.info()));
                }

                PermissionOutcome { decision, reason }
            }
        }
    }
}
