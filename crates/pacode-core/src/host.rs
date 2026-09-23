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
    /// `WaitingApproval`, awaits the decision (cancel → Deny + "interrupted"),
    /// emits `PermissionResolved`, caches `AllowSession`.
    ///
    /// The `reason` in the outcome is `Some` whenever a `Deny` was not the
    /// user's refusal: the gate's policy reason, `zdr_blocked: …`,
    /// `interrupted`, `permission request aborted` (sender dropped), or a
    /// client-supplied note from `PermissionReply`. `require_permission` uses
    /// it to tell the model a policy deny apart from a user refusal.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionOutcome {
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
        if self.session.zdr_active()
            && (kind == pacode_tools::ToolKind::Network || tool_name.contains("__"))
        {
            return PermissionOutcome {
                decision: PermissionDecision::Deny,
                reason: Some(format!(
                    "zdr_blocked: {tool_name} is disabled while zero-data-retention is active"
                )),
            };
        }

        let mode = self.session.meta().mode;
        let allow_catastrophic = self.session.config.permissions.allow_catastrophic;
        let gate_decision = crate::permissions::gate(mode, kind, draft.risk, allow_catastrophic);

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
                let target = draft.detail.lines().next().unwrap_or("").trim();
                let cache_key = crate::permissions::session_key(&tool_name, target);
                if self.session.permissions.is_allowed_for_session(&cache_key) {
                    return PermissionOutcome {
                        decision: PermissionDecision::AllowSession,
                        reason: None,
                    };
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
                self.session.emit_session_status();
                crate::hooks::notification(
                    &self.session,
                    &draft.agent,
                    Some(perm_req.tool.clone()),
                );

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

                // `biased` keeps the interrupt arm in front: when the token
                // fired AND the sender was dropped by `cancel_for_agent`, the
                // reason is "interrupted", not "aborted" — matching the
                // `PermissionResolved` the cancel path already emitted.
                let (decision, reason) = match cancel_token {
                    Some(token) => {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                self.session.permissions.cancel(&perm_id);
                                (
                                    PermissionDecision::Deny,
                                    Some("interrupted".to_string()),
                                )
                            }
                            res = rx => res.unwrap_or_else(|_| {
                                (
                                    PermissionDecision::Deny,
                                    Some("permission request aborted".to_string()),
                                )
                            }),
                        }
                    }
                    None => rx.await.unwrap_or_else(|_| {
                        (
                            PermissionDecision::Deny,
                            Some("permission request aborted".to_string()),
                        )
                    }),
                };

                // `PermissionResolved` + the transcript Permission cell →
                // Notice, shared with `Session::cancel_permissions_for`.
                self.session.emit_permission_resolution(
                    &perm_req,
                    Some(perm_item.seq),
                    decision,
                    reason.clone(),
                );

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

    async fn ask_question(
        &self,
        call_id: &pacode_types::CallId,
        header: String,
        question: String,
        options: Vec<pacode_types::QuestionOption>,
        multi_select: bool,
    ) -> Result<pacode_types::QuestionAnswer, ToolError> {
        let agent_id = self.agent.clone();
        let agent_name = self
            .session
            .agent(&agent_id)
            .map(|a| a.info().name)
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

        // The question goes into the transcript so the conversation shows what was
        // asked even after it is answered, and as an event so the client can put a
        // picker in front of the user.
        let agent_opt = self.session.agent(&agent_id);
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
        if self.session.zdr_active() {
            return Err(ToolError::Denied(
                "zdr_blocked: subagent spawning is disabled while zero-data-retention is active"
                    .to_string(),
            ));
        }
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
