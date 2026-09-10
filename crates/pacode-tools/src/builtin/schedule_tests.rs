use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::state::PermissionDecision;
use pacode_types::{AgentId, CallId, Mode, SessionId};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::host::{AgentSpec, PermissionDraft, WaitOutcome};
use crate::{ToolCtx, ToolHost};

/// A host that only has to answer the scheduling calls; everything else a tool
/// might reach for is refused loudly rather than pretending to work.
#[derive(Default)]
struct SchedHost {
    schedule: crate::test_support::ScheduleStub,
}

#[async_trait]
impl ToolHost for SchedHost {
    async fn request_permission(&self, _draft: PermissionDraft) -> PermissionDecision {
        PermissionDecision::AllowOnce
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
        _spec: pacode_exec::TaskSpec,
    ) -> Result<pacode_types::TaskId, ToolError> {
        Err(ToolError::Failed("no tasks in this test".to_string()))
    }

    fn task_info(&self, _task: &pacode_types::TaskId) -> Option<pacode_types::TaskInfo> {
        None
    }

    fn list_tasks(&self) -> Vec<pacode_types::TaskInfo> {
        Vec::new()
    }

    async fn wait_task(
        &self,
        _task: &pacode_types::TaskId,
        _timeout: Duration,
        _return_on_progress: bool,
    ) -> WaitOutcome {
        WaitOutcome::Timeout
    }

    async fn kill_task(&self, _task: &pacode_types::TaskId) -> Result<(), ToolError> {
        Err(ToolError::Failed("no tasks in this test".to_string()))
    }

    async fn task_tail(
        &self,
        _task: &pacode_types::TaskId,
        _lines: usize,
    ) -> Result<Vec<String>, ToolError> {
        Ok(Vec::new())
    }

    fn report_task_progress(
        &self,
        _task: &pacode_types::TaskId,
        _progress: pacode_types::TaskProgress,
    ) -> Result<(), ToolError> {
        Ok(())
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

    async fn wait_agent(&self, _agent: &AgentId, _timeout: Duration) -> WaitOutcome {
        WaitOutcome::Timeout
    }

    async fn stop_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
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

fn ctx_for(host: Arc<SchedHost>) -> ToolCtx {
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
    }
}

#[tokio::test]
async fn cron_add_registers_a_job_and_reports_the_next_run() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    let out = CronTool
        .call(
            json!({
                "action": "add",
                "name": "nightly",
                "schedule": "every 30m",
                "prompt": "run the suite",
                "intent": "schedule the suite"
            }),
            &ctx,
        )
        .await
        .expect("add");

    assert!(out.content.contains("job_id: cron_"), "{}", out.content);
    assert!(
        out.content.contains("schedule: every 30m"),
        "{}",
        out.content
    );
    assert_eq!(host.schedule.list_cron_jobs().len(), 1);
}

#[tokio::test]
async fn cron_rejects_a_schedule_it_cannot_read() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    let err = CronTool
        .call(
            json!({
                "action": "add",
                "name": "broken",
                "schedule": "sometimes",
                "prompt": "x",
                "intent": "try a bad schedule"
            }),
            &ctx,
        )
        .await
        .expect_err("must be refused");
    assert!(format!("{err}").contains("unrecognised schedule"), "{err}");
    assert!(host.schedule.list_cron_jobs().is_empty());
}

#[tokio::test]
async fn cron_list_and_remove_round_trip() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    CronTool
        .call(
            json!({"action": "add", "name": "nightly", "schedule": "every 1h", "prompt": "x", "intent": "add"}),
            &ctx,
        )
        .await
        .expect("add");
    let id = host.schedule.list_cron_jobs()[0].id.clone();

    let listed = CronTool
        .call(json!({"action": "list", "intent": "list"}), &ctx)
        .await
        .expect("list");
    assert!(listed.content.contains("nightly"), "{}", listed.content);

    CronTool
        .call(
            json!({"action": "remove", "job_id": id.to_string(), "intent": "remove"}),
            &ctx,
        )
        .await
        .expect("remove");
    assert!(host.schedule.list_cron_jobs().is_empty());

    let empty = CronTool
        .call(json!({"action": "list", "intent": "list"}), &ctx)
        .await
        .expect("list");
    assert_eq!(empty.content, "no cron jobs");
}

#[tokio::test]
async fn cron_needs_every_field_of_an_add() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    for (args, missing) in [
        (
            json!({"action": "add", "schedule": "every 1h", "prompt": "x", "intent": "i"}),
            "name",
        ),
        (
            json!({"action": "add", "name": "n", "prompt": "x", "intent": "i"}),
            "schedule",
        ),
        (
            json!({"action": "add", "name": "n", "schedule": "every 1h", "intent": "i"}),
            "prompt",
        ),
    ] {
        let err = CronTool
            .call(args, &ctx)
            .await
            .expect_err("must be refused");
        assert!(
            format!("{err}").contains(missing),
            "{err} should mention {missing}"
        );
    }

    let err = CronTool
        .call(json!({"action": "dance", "intent": "i"}), &ctx)
        .await
        .expect_err("unknown action");
    assert!(format!("{err}").contains("unknown action"), "{err}");
}

#[tokio::test]
async fn monitor_watch_accepts_each_condition_and_needs_its_fields() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    for args in [
        json!({"action": "watch", "condition": "command_succeeds", "command": "test -f done", "intent": "i"}),
        json!({"action": "watch", "condition": "process_gone", "process": "dota2", "intent": "i"}),
        json!({"action": "watch", "condition": "file_exists", "path": "target/release/pacode", "intent": "i"}),
        json!({"action": "watch", "condition": "file_matches", "path": "build.log", "pattern": "Finished", "intent": "i"}),
    ] {
        MonitorTool.call(args, &ctx).await.expect("watch");
    }
    assert_eq!(host.schedule.list_monitors().len(), 4);

    let err = MonitorTool
        .call(
            json!({"action": "watch", "condition": "file_matches", "path": "x", "intent": "i"}),
            &ctx,
        )
        .await
        .expect_err("missing pattern");
    assert!(format!("{err}").contains("pattern"), "{err}");

    let err = MonitorTool
        .call(
            json!({"action": "watch", "condition": "telepathy", "intent": "i"}),
            &ctx,
        )
        .await
        .expect_err("unknown condition");
    assert!(format!("{err}").contains("unknown condition"), "{err}");
}

#[tokio::test]
async fn monitor_list_and_stop_round_trip() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    MonitorTool
        .call(
            json!({"action": "watch", "label": "build", "condition": "file_exists", "path": "done", "intent": "i"}),
            &ctx,
        )
        .await
        .expect("watch");
    let id = host.schedule.list_monitors()[0].id.clone();

    let listed = MonitorTool
        .call(json!({"action": "list", "intent": "i"}), &ctx)
        .await
        .expect("list");
    assert!(listed.content.contains("build"), "{}", listed.content);

    MonitorTool
        .call(
            json!({"action": "stop", "monitor_id": id.to_string(), "intent": "i"}),
            &ctx,
        )
        .await
        .expect("stop");
    assert!(
        host.schedule.list_monitors()[0].status.is_terminal(),
        "a stopped monitor must be terminal"
    );
}

#[tokio::test]
async fn long_values_from_the_model_are_capped() {
    let host = Arc::new(SchedHost::default());
    let ctx = ctx_for(host.clone());

    CronTool
        .call(
            json!({
                "action": "add",
                "name": "n".repeat(NAME_MAX_CHARS + 100),
                "schedule": "every 1h",
                "prompt": "p".repeat(PROMPT_MAX_CHARS + 100),
                "intent": "i"
            }),
            &ctx,
        )
        .await
        .expect("add");

    let job = host.schedule.list_cron_jobs()[0].clone();
    assert_eq!(job.name.chars().count(), NAME_MAX_CHARS);
    assert_eq!(job.prompt.chars().count(), PROMPT_MAX_CHARS);
}
