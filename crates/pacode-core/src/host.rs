//! `ToolHost` implementation: the session as seen by tools.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::TaskSpec;
use pacode_tools::host::PermissionDraft;
use pacode_tools::{AgentSpec, ToolError, ToolHost, WaitOutcome};
use pacode_types::{
    AgentId, AgentInfo, CallId, PermissionDecision, Plan, TaskId, TaskInfo, TaskProgress,
    ToastLevel,
};

use crate::session::Session;

pub struct SessionHost {
    pub session: Arc<Session>,
    /// The agent making the calls (its mode/tools decide the gate).
    pub agent: AgentId,
}

#[async_trait]
impl ToolHost for SessionHost {
    /// Applies `permissions::gate` with the session mode and the draft's risk; checks
    /// the AllowSession cache; otherwise registers the request, emits
    /// `PermissionRequested` + an `ItemAdded(Permission)` item, sets the agent to
    /// `WaitingApproval`, awaits the decision (cancel → Deny), emits
    /// `PermissionResolved`, caches `AllowSession`.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionDecision {
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

        let mode = self.session.meta().mode;
        let allow_catastrophic = self.session.config.permissions.allow_catastrophic;
        let gate_decision = crate::permissions::gate(mode, kind, draft.risk, allow_catastrophic);

        match gate_decision {
            crate::permissions::GateDecision::Allow => PermissionDecision::AllowOnce,
            crate::permissions::GateDecision::Deny(_) => PermissionDecision::Deny,
            crate::permissions::GateDecision::Ask => {
                let target = draft.detail.lines().next().unwrap_or("").trim();
                let cache_key = crate::permissions::session_key(&tool_name, target);
                if self.session.permissions.is_allowed_for_session(&cache_key) {
                    return PermissionDecision::AllowSession;
                }

                let perm_id = pacode_types::PermissionId::generate();
                let perm_req = pacode_types::PermissionRequest {
                    id: perm_id.clone(),
                    agent: draft.agent.clone(),
                    agent_name: draft.agent_name.clone(),
                    call_id: draft.call_id.clone(),
                    tool: tool_name,
                    title: draft.title.clone(),
                    detail: draft.detail.clone(),
                    risk: draft.risk,
                    created_at_ms: pacode_types::now_ms(),
                };

                let rx = self.session.permissions.register(perm_req.clone());
                self.session
                    .events
                    .emit(pacode_types::Event::PermissionRequested(perm_req.clone()));

                let agent_opt = self.session.agent(&draft.agent);
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
                    agent: draft.agent.clone(),
                    ts_ms: perm_req.created_at_ms,
                    kind: pacode_types::TranscriptKind::Permission(perm_req),
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

                let prev_status = agent_opt
                    .as_ref()
                    .map(|a| a.status())
                    .unwrap_or(pacode_types::AgentStatus::Idle);
                let prev_activity = agent_opt.as_ref().and_then(|a| a.info().activity);

                if let Some(agent) = &agent_opt {
                    agent.set_status(
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

                let decision = match cancel_token {
                    Some(token) => {
                        tokio::select! {
                            _ = token.cancelled() => {
                                self.session.permissions.cancel(&perm_id);
                                PermissionDecision::Deny
                            }
                            res = rx => res.unwrap_or(PermissionDecision::Deny),
                        }
                    }
                    None => rx.await.unwrap_or(PermissionDecision::Deny),
                };

                self.session
                    .events
                    .emit(pacode_types::Event::PermissionResolved {
                        permission: perm_id,
                        decision,
                    });

                if decision == PermissionDecision::AllowSession {
                    self.session.permissions.allow_for_session(cache_key);
                }

                let (level, notice_text) = match decision {
                    PermissionDecision::AllowOnce => (
                        pacode_types::ToastLevel::Info,
                        "Permission allowed".to_string(),
                    ),
                    PermissionDecision::AllowSession => (
                        pacode_types::ToastLevel::Info,
                        "Permission allowed for session".to_string(),
                    ),
                    PermissionDecision::Deny => (
                        pacode_types::ToastLevel::Warn,
                        "Permission denied".to_string(),
                    ),
                };
                let notice_item = pacode_types::TranscriptItem {
                    seq: perm_item.seq,
                    agent: draft.agent.clone(),
                    ts_ms: pacode_types::now_ms(),
                    kind: pacode_types::TranscriptKind::Notice {
                        level,
                        text: notice_text,
                    },
                };
                if let Some(agent) = &agent_opt {
                    agent
                        .transcript
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .upsert(notice_item.clone());
                }
                self.session
                    .events
                    .emit(pacode_types::Event::ItemUpdated(notice_item));

                if let Some(agent) = &agent_opt {
                    agent.set_status(prev_status, prev_activity);
                    self.session
                        .events
                        .emit(pacode_types::Event::AgentUpdated(agent.info()));
                }

                decision
            }
        }
    }

    async fn add_cron_job(
        &self,
        name: String,
        schedule: pacode_types::CronSchedule,
        prompt: String,
    ) -> Result<pacode_types::CronJob, ToolError> {
        let job = self
            .session
            .scheduler
            .add_job(name, schedule, prompt, pacode_types::time::now_ms())
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        if let Err(e) = self
            .session
            .store
            .upsert_cron_job(&self.session.id, &job)
            .await
        {
            log::warn!("failed to persist cron job {}: {e}", job.id);
        }
        self.session
            .events
            .emit(pacode_types::Event::CronUpdated(job.clone()));
        Ok(job)
    }

    fn list_cron_jobs(&self) -> Vec<pacode_types::CronJob> {
        self.session.scheduler.jobs()
    }

    async fn remove_cron_job(&self, id: &pacode_types::CronJobId) -> Result<(), ToolError> {
        self.session
            .scheduler
            .remove_job(id)
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        if let Err(e) = self.session.store.delete_cron_job(id).await {
            log::warn!("failed to delete cron job {id}: {e}");
        }
        self.session
            .events
            .emit(pacode_types::Event::CronRemoved(id.clone()));
        Ok(())
    }

    fn add_monitor(
        &self,
        label: String,
        condition: pacode_types::MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<pacode_types::MonitorInfo, ToolError> {
        let info = self
            .session
            .scheduler
            .add_monitor(
                label,
                condition,
                poll_interval_secs,
                pacode_types::time::now_ms(),
            )
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        self.session
            .events
            .emit(pacode_types::Event::MonitorUpdated(info.clone()));
        Ok(info)
    }

    fn list_monitors(&self) -> Vec<pacode_types::MonitorInfo> {
        self.session.scheduler.monitors()
    }

    fn stop_monitor(&self, id: &pacode_types::MonitorId) -> Result<(), ToolError> {
        let info = self
            .session
            .scheduler
            .stop_monitor(id)
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        self.session
            .events
            .emit(pacode_types::Event::MonitorUpdated(info));
        Ok(())
    }

    async fn spawn_task(&self, spec: TaskSpec) -> Result<TaskId, ToolError> {
        let info = self
            .session
            .tasks
            .spawn(spec)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(info.id)
    }

    fn task_info(&self, task: &TaskId) -> Option<TaskInfo> {
        self.session.tasks.info(task)
    }

    fn list_tasks(&self) -> Vec<TaskInfo> {
        self.session.tasks_of_session()
    }

    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome {
        let res = self
            .session
            .tasks
            .wait(task, timeout, return_on_progress)
            .await;
        match res {
            pacode_exec::WaitResult::Ended(_) => WaitOutcome::Finished,
            pacode_exec::WaitResult::Progress(_) => WaitOutcome::Progress,
            pacode_exec::WaitResult::Timeout(_) => WaitOutcome::Timeout,
            pacode_exec::WaitResult::NotFound => WaitOutcome::Finished,
        }
    }

    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError> {
        self.session
            .tasks
            .kill(task)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError> {
        self.session
            .tasks
            .tail(task, lines)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError> {
        self.session
            .tasks
            .report_progress(task, progress)
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    async fn spawn_agent(&self, spec: AgentSpec) -> Result<AgentId, ToolError> {
        self.session
            .spawn_agent(spec)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo> {
        self.session.agent(agent).map(|a| a.info())
    }

    fn list_agents(&self) -> Vec<AgentInfo> {
        self.session.agent_infos()
    }

    async fn wait_agent(&self, agent: &AgentId, timeout: Duration) -> WaitOutcome {
        if let Some(agent_obj) = self.session.agent(agent) {
            if !agent_obj.status().is_live() {
                return WaitOutcome::Finished;
            }
        } else {
            return WaitOutcome::Finished;
        }

        if timeout.is_zero() {
            return WaitOutcome::Timeout;
        }

        let mut events_rx = self.session.events.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return WaitOutcome::Timeout;
            }

            match tokio::time::timeout(remaining, events_rx.recv()).await {
                Ok(Ok((_seq, event))) => match event {
                    pacode_types::Event::AgentUpdated(info) if info.id == *agent => {
                        if !info.status.is_live() {
                            return WaitOutcome::Finished;
                        }
                    }
                    pacode_types::Event::TurnEnded { agent: a, .. } if a == *agent => {
                        return WaitOutcome::Finished;
                    }
                    _ => {}
                },
                Ok(Err(_)) => {
                    if let Some(a) = self.session.agent(agent)
                        && !a.status().is_live()
                    {
                        return WaitOutcome::Finished;
                    }
                    return WaitOutcome::Timeout;
                }
                Err(_) => return WaitOutcome::Timeout,
            }
        }
    }

    fn request_agent_status(&self, agent: &AgentId) -> Result<(), ToolError> {
        let target = self
            .session
            .agent(agent)
            .ok_or_else(|| ToolError::failed(format!("agent not found: {agent}")))?;
        if !target.info().status.is_live() {
            return Err(ToolError::failed(format!("agent {agent} is not running")));
        }
        target
            .injections
            .push(crate::inject::Injection::StatusRequest);
        Ok(())
    }

    fn report_status(&self, text: String) -> Result<(), ToolError> {
        let me = self
            .session
            .agent(&self.agent)
            .ok_or_else(|| ToolError::failed("agent not found"))?;
        let info = me.info();
        let parent_id = info
            .parent
            .clone()
            .ok_or_else(|| ToolError::invalid("only subagents can report status"))?;
        let parent = self
            .session
            .agent(&parent_id)
            .ok_or_else(|| ToolError::failed("parent agent is gone"))?;
        parent
            .injections
            .push(crate::inject::Injection::AgentStatus { agent: info, text });
        if parent.id().is_main() && !parent.is_running() {
            crate::turn::start_turn(self.session.clone(), parent);
        }
        Ok(())
    }

    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError> {
        self.session
            .stop_agent(agent)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    fn plan(&self) -> Plan {
        self.session
            .plan
            .read()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    fn set_plan(&self, plan: Plan) {
        self.session.set_plan(plan);
    }

    fn emit_preview(&self, call_id: &CallId, preview: String) {
        if let Some(agent) = self.session.agent(&self.agent) {
            let mut transcript = agent.transcript.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(item) = transcript.tool_items.get_mut(call_id) {
                if let pacode_types::TranscriptKind::ToolCall {
                    preview: ref mut p, ..
                } = item.kind
                {
                    *p = preview;
                }
                let updated = item.clone();
                transcript.upsert(updated.clone());
                drop(transcript);
                self.session
                    .events
                    .emit(pacode_types::Event::ItemUpdated(updated));
            }
        }
    }

    fn emit_notice(&self, level: ToastLevel, text: String) {
        if let Some(agent) = self.session.agent(&self.agent) {
            let seq = agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .next_seq();
            let item = pacode_types::TranscriptItem {
                seq,
                agent: self.agent.clone(),
                ts_ms: pacode_types::now_ms(),
                kind: pacode_types::TranscriptKind::Notice { level, text },
            };
            agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .upsert(item.clone());
            self.session
                .events
                .emit(pacode_types::Event::ItemAdded(item));
        }
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod host_tests;
