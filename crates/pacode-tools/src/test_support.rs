//! In-memory scheduling state for hosts written in tests.
//!
//! `ToolHost` is the only door from a tool into the core, so every stub host in a
//! test has to answer the scheduling calls too. This keeps those answers real —
//! jobs and monitors are actually stored and listed back — without each test
//! rebuilding the same bookkeeping.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::state::PermissionDecision;
use pacode_types::{
    AgentId, CallId, CronJob, CronJobId, CronSchedule, Mode, ModelRoute, MonitorCondition,
    MonitorId, MonitorInfo, MonitorStatus, SessionId,
};
use tokio_util::sync::CancellationToken;

use crate::ToolError;
use crate::host::{AgentKindDef, AgentSpec, PermissionDraft, ToolCtx, ToolHost, WaitOutcome};

#[derive(Default)]
pub struct ScheduleStub {
    jobs: Mutex<Vec<CronJob>>,
    monitors: Mutex<Vec<MonitorInfo>>,
}

impl ScheduleStub {
    pub fn add_cron_job(
        &self,
        name: String,
        schedule: CronSchedule,
        prompt: String,
    ) -> Result<CronJob, ToolError> {
        let now = pacode_types::time::now_ms();
        let mut job = CronJob {
            id: CronJobId::generate(),
            name,
            schedule,
            prompt,
            enabled: true,
            created_at_ms: now,
            last_run_ms: None,
            next_run_ms: None,
            last_status: None,
        };
        job.reschedule(now)
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        self.jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(job.clone());
        Ok(job)
    }

    pub fn list_cron_jobs(&self) -> Vec<CronJob> {
        self.jobs
            .lock()
            .map(|j| j.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn remove_cron_job(&self, id: &CronJobId) -> Result<(), ToolError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let before = jobs.len();
        jobs.retain(|j| &j.id != id);
        if jobs.len() == before {
            return Err(ToolError::invalid(format!("unknown cron job {id}")));
        }
        Ok(())
    }

    pub fn add_monitor(
        &self,
        label: String,
        condition: MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<MonitorInfo, ToolError> {
        let info = MonitorInfo {
            id: MonitorId::generate(),
            label,
            condition,
            poll_interval_secs: MonitorInfo::normalize_poll_interval(poll_interval_secs),
            started_at_ms: pacode_types::time::now_ms(),
            status: MonitorStatus::Watching,
            last_check_ms: None,
            fired_at_ms: None,
        };
        self.monitors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(info.clone());
        Ok(info)
    }

    pub fn list_monitors(&self) -> Vec<MonitorInfo> {
        self.monitors
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn stop_monitor(&self, id: &MonitorId) -> Result<(), ToolError> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        let monitor = monitors
            .iter_mut()
            .find(|m| &m.id == id)
            .ok_or_else(|| ToolError::invalid(format!("unknown monitor {id}")))?;
        monitor.status = MonitorStatus::Stopped;
        Ok(())
    }
}

/// A host that only has to answer the scheduling calls; everything else a tool
/// might reach for is refused loudly rather than pretending to work.
///
/// Permission drafts are recorded in `drafts` and answered with `decision`
/// (default `AllowOnce`) plus the optional deny `reason`. When `tasks` is set,
/// task calls delegate to a real `TaskManager`, so tests can run actual
/// commands.
pub struct StubToolHost {
    pub schedule: ScheduleStub,
    pub drafts: Mutex<Vec<PermissionDraft>>,
    pub decision: Mutex<PermissionDecision>,
    pub reason: Mutex<Option<String>>,
    pub tasks: Option<Arc<pacode_exec::TaskManager>>,
}

impl Default for StubToolHost {
    fn default() -> Self {
        Self {
            schedule: ScheduleStub::default(),
            drafts: Mutex::new(Vec::new()),
            decision: Mutex::new(PermissionDecision::AllowOnce),
            reason: Mutex::new(None),
            tasks: None,
        }
    }
}

#[async_trait]
impl ToolHost for StubToolHost {
    async fn request_permission(&self, draft: PermissionDraft) -> crate::host::PermissionOutcome {
        self.drafts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(draft);
        crate::host::PermissionOutcome {
            decision: *self.decision.lock().unwrap_or_else(|p| p.into_inner()),
            reason: self
                .reason
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        }
    }

    async fn ask_question(
        &self,
        call_id: &CallId,
        header: String,
        question: String,
        options: Vec<pacode_types::QuestionOption>,
        multi_select: bool,
    ) -> Result<pacode_types::QuestionAnswer, ToolError> {
        // Build the question exactly as the real host does, so a shape the picker
        // could not present fails here too instead of only in production.
        let built = pacode_types::Question::new(
            pacode_types::QuestionId::generate(),
            pacode_types::QuestionOrigin::new(AgentId::main(), "main", call_id.clone()),
            header,
            question,
            options,
            multi_select,
            0,
        )
        .map_err(|e| ToolError::invalid(e.to_string()))?;

        // The stub user takes the recommended option, or the first one.
        Ok(pacode_types::QuestionAnswer::choice(
            built.recommended_index().unwrap_or(0),
        ))
    }

    async fn add_cron_job(
        &self,
        name: String,
        schedule: pacode_types::CronSchedule,
        prompt: String,
    ) -> Result<pacode_types::CronJob, ToolError> {
        self.schedule.add_cron_job(name, schedule, prompt)
    }

    fn list_cron_jobs(&self) -> Vec<pacode_types::CronJob> {
        self.schedule.list_cron_jobs()
    }

    async fn remove_cron_job(&self, id: &pacode_types::CronJobId) -> Result<(), ToolError> {
        self.schedule.remove_cron_job(id)
    }

    fn add_monitor(
        &self,
        label: String,
        condition: pacode_types::MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<pacode_types::MonitorInfo, ToolError> {
        self.schedule
            .add_monitor(label, condition, poll_interval_secs)
    }

    fn list_monitors(&self) -> Vec<pacode_types::MonitorInfo> {
        self.schedule.list_monitors()
    }

    fn stop_monitor(&self, id: &pacode_types::MonitorId) -> Result<(), ToolError> {
        self.schedule.stop_monitor(id)
    }

    async fn spawn_task(
        &self,
        spec: pacode_exec::TaskSpec,
    ) -> Result<pacode_types::TaskId, ToolError> {
        match &self.tasks {
            Some(tasks) => tasks
                .spawn(spec)
                .await
                .map(|info| info.id)
                .map_err(|e| ToolError::failed(e.to_string())),
            None => Err(ToolError::Failed("no tasks in this test".to_string())),
        }
    }

    fn task_info(&self, task: &pacode_types::TaskId) -> Option<pacode_types::TaskInfo> {
        self.tasks.as_ref().and_then(|t| t.info(task))
    }

    fn list_tasks(&self) -> Vec<pacode_types::TaskInfo> {
        self.tasks
            .as_ref()
            .map(|t| t.list(None))
            .unwrap_or_default()
    }

    async fn wait_task(
        &self,
        task: &pacode_types::TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome {
        match &self.tasks {
            Some(tasks) => match tasks.wait(task, timeout, return_on_progress).await {
                pacode_exec::WaitResult::Ended(_) | pacode_exec::WaitResult::NotFound => {
                    WaitOutcome::Finished
                }
                pacode_exec::WaitResult::Progress(_) => WaitOutcome::Progress,
                pacode_exec::WaitResult::Timeout(_) => WaitOutcome::Timeout,
            },
            None => WaitOutcome::Timeout,
        }
    }

    async fn kill_task(&self, task: &pacode_types::TaskId) -> Result<(), ToolError> {
        match &self.tasks {
            Some(tasks) => tasks
                .kill(task)
                .await
                .map_err(|e| ToolError::failed(e.to_string())),
            None => Err(ToolError::Failed("no tasks in this test".to_string())),
        }
    }

    async fn task_tail(
        &self,
        task: &pacode_types::TaskId,
        lines: usize,
    ) -> Result<Vec<String>, ToolError> {
        match &self.tasks {
            Some(tasks) => tasks
                .tail(task, lines)
                .await
                .map_err(|e| ToolError::failed(e.to_string())),
            None => Ok(Vec::new()),
        }
    }

    fn report_task_progress(
        &self,
        task: &pacode_types::TaskId,
        progress: pacode_types::TaskProgress,
    ) -> Result<(), ToolError> {
        match &self.tasks {
            Some(tasks) => tasks
                .report_progress(task, progress)
                .map_err(|e| ToolError::failed(e.to_string())),
            None => Ok(()),
        }
    }

    async fn spawn_agent(&self, _spec: AgentSpec) -> Result<AgentId, ToolError> {
        Err(ToolError::Failed("no agents in this test".to_string()))
    }

    fn agent_info(&self, _agent: &AgentId) -> Option<pacode_types::AgentInfo> {
        None
    }

    fn list_agents(&self) -> Vec<pacode_types::AgentInfo> {
        Vec::new()
    }

    fn agent_kinds(&self) -> Vec<AgentKindDef> {
        Vec::new()
    }

    fn parse_model_route(&self, s: &str) -> Option<ModelRoute> {
        ModelRoute::parse_lossy(s)
    }

    async fn stop_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Err(ToolError::Failed("no agents in this test".to_string()))
    }

    async fn continue_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Err(ToolError::Failed("no agents in this test".to_string()))
    }

    fn request_agent_status(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Err(ToolError::Failed("no agents in this test".to_string()))
    }

    fn report_status(&self, _text: String) -> Result<(), ToolError> {
        Ok(())
    }

    fn plan(&self) -> pacode_types::Plan {
        pacode_types::Plan::default()
    }

    fn set_plan(&self, _plan: pacode_types::Plan) {}

    fn emit_preview(&self, _call_id: &CallId, _preview: String) {}

    fn emit_notice(&self, _level: pacode_types::ToastLevel, _text: String) {}
}

pub fn stub_ctx(host: Arc<StubToolHost>) -> ToolCtx {
    ToolCtx {
        session: SessionId::new("ses_sched"),
        agent: AgentId::main(),
        agent_name: "main".to_string(),
        call_id: CallId::new("call_sched"),
        cwd: std::env::current_dir().unwrap_or_default(),
        mode: Mode::Build,
        host,
        cancel: CancellationToken::new(),
        output_cap_chars: 16_000,
        exec_yield_after: Duration::from_secs(5),
        exec_default_timeout: Duration::from_secs(30),
        tool_name: None,
        tool_kind: None,
        tools: crate::registry::ToolRegistry::new(),
    }
}
