//! `ToolHost` implementation: the session as seen by tools.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::TaskSpec;
use pacode_tools::host::{AgentKindDef, PermissionDraft, PermissionOutcome};
use pacode_tools::{AgentSpec, ToolError, ToolHost, WaitOutcome};
use pacode_types::{
    AgentId, AgentInfo, CallId, ModelRoute, PermissionDecision, Plan, TaskId, TaskInfo,
    TaskProgress, ToastLevel,
};

use crate::agent::Agent;
use crate::session::Session;

pub struct SessionHost {
    pub session: Arc<Session>,
    /// The agent making the calls (its mode/tools decide the gate).
    pub agent: AgentId,
}

impl SessionHost {
    fn owner_agent(&self) -> Option<Arc<Agent>> {
        self.session.agent_for_host(&self.agent)
    }

    fn owner_id(&self) -> AgentId {
        self.owner_agent()
            .map(|agent| agent.id())
            .unwrap_or_else(|| self.agent.clone())
    }

    fn task_info(&self, task: &TaskId) -> Option<TaskInfo> {
        self.session
            .tasks
            .info(task)
            .filter(|info| info.session == self.session.id)
    }
}

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

fn zdr_denied_subagent() -> ToolError {
    ToolError::Denied(format!(
        "{}: subagent spawning is disabled while zero-data-retention is active",
        crate::permissions::ZDR_BLOCKED
    ))
}

impl SessionHost {
    async fn request_permission_inner(
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
                    // restoring the pre-permission status.
                    if !agent.stop_requested() {
                        agent.set_status(prev_status, prev_activity);
                    }
                    self.session
                        .events
                        .emit(pacode_types::Event::AgentUpdated(agent.info()));
                }

                PermissionOutcome { decision, reason }
            }
        }
    }
}

#[async_trait]
impl ToolHost for SessionHost {
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionOutcome {
        self.request_permission_inner(draft, false).await
    }

    async fn ask_question(
        &self,
        call_id: &pacode_types::CallId,
        header: String,
        question: String,
        options: Vec<pacode_types::QuestionOption>,
        multi_select: bool,
    ) -> Result<pacode_types::QuestionAnswer, ToolError> {
        let owner = self.owner_agent();
        let agent_id = owner
            .as_ref()
            .map(|agent| agent.id())
            .unwrap_or_else(|| self.agent.clone());
        let agent_name = owner
            .as_ref()
            .map(|agent| {
                agent
                    .info
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .name
                    .clone()
            })
            .unwrap_or_else(|| "main".to_string());

        let question = pacode_types::Question::new(
            pacode_types::QuestionId::generate(),
            pacode_types::QuestionOrigin::new(agent_id.clone(), agent_name, call_id.clone()),
            header,
            question,
            options,
            multi_select,
            pacode_types::now_ms(),
        )
        .map_err(|e| ToolError::invalid(e.to_string()))?;

        let Some(rx) = self.session.questions.register(question.clone()) else {
            return Err(ToolError::Failed(
                "too many questions are already waiting for an answer".to_string(),
            ));
        };
        let cancelled_before_publish = owner
            .as_ref()
            .and_then(|agent| agent.cancel.lock().ok().and_then(|token| token.clone()))
            .is_some_and(|token| token.is_cancelled());
        if cancelled_before_publish {
            self.session.cancel_questions_for(&agent_id);
            return Ok(pacode_types::QuestionAnswer::cancelled());
        }

        // The question goes into the transcript so the conversation shows what was
        // asked even after it is answered, and as an event so the client can put a
        // picker in front of the user.
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
        let item = pacode_types::TranscriptItem {
            seq: item_seq,
            agent: agent_id,
            ts_ms: question.created_at_ms,
            kind: pacode_types::TranscriptKind::Question {
                question: question.clone(),
                answer: None,
            },
        };
        if let Some(agent) = &agent_opt {
            agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .upsert(item.clone());
        }
        self.session
            .events
            .emit(pacode_types::Event::ItemAdded(item));
        self.session
            .events
            .emit(pacode_types::Event::QuestionAsked(question.clone()));
        self.session.emit_session_status();
        crate::hooks::notification(&self.session, &question.agent, None);

        // A client that goes away, or an agent that is stopped, drops the sender:
        // the call comes back cancelled rather than waiting for an answer that
        // will never arrive.
        match rx.await {
            Ok(answer) => Ok(answer),
            Err(_) => Ok(pacode_types::QuestionAnswer::cancelled()),
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

    async fn spawn_task(&self, mut spec: TaskSpec) -> Result<TaskId, ToolError> {
        if spec.session != self.session.id {
            return Err(ToolError::Failed(
                "task does not belong to this session".to_string(),
            ));
        }
        spec.owner = self.owner_id();
        let info = self
            .session
            .tasks
            .spawn(spec)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        if let Err(error) = self.session.store.upsert_task(&info).await {
            let _ = self.session.tasks.kill(&info.id).await;
            return Err(ToolError::Failed(error.to_string()));
        }
        Ok(info.id)
    }

    fn task_info(&self, task: &TaskId) -> Option<TaskInfo> {
        self.task_info(task)
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
        if self.task_info(task).is_none() {
            return WaitOutcome::Finished;
        }
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
        if self.task_info(task).is_none() {
            return Err(ToolError::Failed("task not found".to_string()));
        }
        self.session
            .tasks
            .kill(task)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError> {
        if self.task_info(task).is_none() {
            return Err(ToolError::Failed("task not found".to_string()));
        }
        self.session
            .tasks
            .tail(task, lines)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError> {
        if self.task_info(task).is_none() {
            return Err(ToolError::Failed("task not found".to_string()));
        }
        self.session
            .tasks
            .report_progress(task, progress)
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    async fn spawn_agent(&self, mut spec: AgentSpec) -> Result<AgentId, ToolError> {
        if self.session.zdr_active() {
            return Err(zdr_denied_subagent());
        }
        let requested_route = spec.model.take();
        let owner = self
            .owner_agent()
            .ok_or_else(|| ToolError::failed("agent not found"))?;
        let (owner_id, owner_name, owner_route) = {
            let info = owner.info.read().unwrap_or_else(|p| p.into_inner());
            (info.id.clone(), info.name.clone(), info.model.clone())
        };
        // The pair this spawn will be judged by: the orchestrator route it
        // departs from, and the route the subagent would actually run on.
        let authorized_from = owner_route;
        let asked_route = requested_route
            .clone()
            .unwrap_or_else(|| authorized_from.clone());
        if asked_route != authorized_from {
            let outcome = self
                .request_permission_inner(
                    PermissionDraft {
                        agent: owner_id.clone(),
                        agent_name: owner_name,
                        call_id: CallId::generate(),
                        title: format!("Authorize subagent route {asked_route}"),
                        detail: format!(
                            "Subagent route {asked_route} differs from orchestrator route {authorized_from}"
                        ),
                        risk: None,
                        tool_name: Some("agent.route".to_string()),
                        tool_kind: Some(pacode_tools::ToolKind::Control),
                    },
                    true,
                )
                .await;
            if outcome.decision == PermissionDecision::Deny {
                return Err(ToolError::Denied(
                    outcome
                        .reason
                        .unwrap_or_else(|| "subagent route change denied".to_string()),
                ));
            }
        }

        // Commit point. The user answers that prompt in their own time, and both
        // the session route and the ZDR flag can move while it is open, so the
        // decision above is re-read here — ZDR and route under one guard, with
        // nothing awaited in between — and anything it does not cover is
        // refused instead of spawned. A toggle landing after this point still
        // fails closed downstream: `set_model` propagates to live subagents
        // still on the old route, and the turn loop re-checks the ZDR flag
        // before every provider call.
        let effective_route = {
            let meta = self.session.meta.read().unwrap_or_else(|p| p.into_inner());
            if self.session.zdr_active() {
                return Err(zdr_denied_subagent());
            }
            let current_route = meta.model.clone();
            let effective = requested_route.unwrap_or_else(|| current_route.clone());
            let covered = effective == current_route
                || (current_route == authorized_from && effective == asked_route);
            if !covered {
                return Err(ToolError::Denied(format!(
                    "session route changed from {authorized_from} to {current_route} while the \
                     subagent route {effective} was being authorized; spawn again to confirm it"
                )));
            }
            effective
        };

        spec.model = Some(effective_route);
        self.session
            .spawn_agent(spec)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo> {
        self.session.agent_for_host(agent).map(|a| a.info())
    }

    fn list_agents(&self) -> Vec<AgentInfo> {
        self.session.agent_infos()
    }

    /// `<cwd>/.pacode/agents` first (project kinds shadow global ones), then the
    /// global `agents/` dir next to `config.toml`. No cache: `spawn`/`list` are
    /// rare and a scan is a directory listing plus small file reads.
    fn agent_kinds(&self) -> Vec<AgentKindDef> {
        let dirs = [
            self.session.meta().cwd.join(".pacode").join("agents"),
            pacode_config::Paths::discover().agents_dir(),
        ];
        let (kinds, warnings) = pacode_tools::host::discover_agent_kinds(&dirs);
        for warning in &warnings {
            log::warn!("{warning}");
        }
        kinds
    }

    fn parse_model_route(&self, s: &str) -> Option<ModelRoute> {
        self.session.providers.parse_route(s)
    }

    fn request_agent_status(&self, agent: &AgentId) -> Result<(), ToolError> {
        let target = self
            .session
            .agent_for_host(agent)
            .ok_or_else(|| ToolError::failed(format!("agent not found: {agent}")))?;
        let is_live = target
            .info
            .read()
            .map(|info| info.status.is_live())
            .unwrap_or_else(|poisoned| poisoned.into_inner().status.is_live());
        if !is_live {
            return Err(ToolError::failed(format!("agent {agent} is not running")));
        }
        target
            .injections
            .push(crate::inject::Injection::StatusRequest);
        Ok(())
    }

    fn report_status(&self, text: String) -> Result<(), ToolError> {
        let me = self
            .owner_agent()
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
        if parent.id().is_main() {
            crate::turn::lifecycle::request_turn(self.session.clone(), parent);
        }
        Ok(())
    }

    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError> {
        self.session
            .stop_agent(agent)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))
    }

    async fn continue_agent(&self, agent: &AgentId) -> Result<(), ToolError> {
        self.session
            .continue_agent(agent)
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
        if let Some(agent) = self.owner_agent() {
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
        if let Some(agent) = self.owner_agent() {
            let seq = agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .next_seq();
            let item = pacode_types::TranscriptItem {
                seq,
                agent: agent.id(),
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
