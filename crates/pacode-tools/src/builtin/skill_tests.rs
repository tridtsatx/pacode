use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_skills::SkillRegistry;
use pacode_types::{
    AgentId, AgentInfo, CallId, Mode, PermissionDecision, Plan, SessionId, TaskId, TaskInfo,
    TaskProgress,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::builtin::skill::SkillTool;
use crate::host::PermissionDraft;
use crate::{AgentSpec, Tool, ToolCtx, ToolError, ToolHost, WaitOutcome};
use pacode_exec::TaskSpec;

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "pacode_tool_skill_test_{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[derive(Default)]
struct DummyHost {
    schedule: crate::test_support::ScheduleStub,
}

#[async_trait]
impl ToolHost for DummyHost {
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

    async fn request_permission(&self, _draft: PermissionDraft) -> PermissionDecision {
        PermissionDecision::AllowOnce
    }
    async fn spawn_task(&self, _spec: TaskSpec) -> Result<TaskId, ToolError> {
        Err(ToolError::failed("unused"))
    }
    fn task_info(&self, _task: &TaskId) -> Option<TaskInfo> {
        None
    }
    fn list_tasks(&self) -> Vec<TaskInfo> {
        Vec::new()
    }
    async fn wait_task(
        &self,
        _task: &TaskId,
        _timeout: Duration,
        _return_on_progress: bool,
    ) -> WaitOutcome {
        WaitOutcome::Finished
    }
    async fn kill_task(&self, _task: &TaskId) -> Result<(), ToolError> {
        Ok(())
    }
    async fn task_tail(&self, _task: &TaskId, _lines: usize) -> Result<Vec<String>, ToolError> {
        Ok(Vec::new())
    }
    fn report_task_progress(
        &self,
        _task: &TaskId,
        _progress: TaskProgress,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    async fn spawn_agent(&self, _spec: AgentSpec) -> Result<AgentId, ToolError> {
        Err(ToolError::failed("unused"))
    }
    fn agent_info(&self, _agent: &AgentId) -> Option<AgentInfo> {
        None
    }
    fn list_agents(&self) -> Vec<AgentInfo> {
        Vec::new()
    }
    async fn wait_agent(&self, _agent: &AgentId, _timeout: Duration) -> WaitOutcome {
        WaitOutcome::Finished
    }
    async fn stop_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }
    fn request_agent_status(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }
    fn report_status(&self, _text: String) -> Result<(), ToolError> {
        Ok(())
    }
    fn plan(&self) -> Plan {
        Plan::default()
    }
    fn set_plan(&self, _plan: Plan) {}
    fn emit_preview(&self, _call_id: &CallId, _preview: String) {}
    fn emit_notice(&self, _level: pacode_types::ToastLevel, _text: String) {}
}

fn make_ctx(cwd: PathBuf) -> ToolCtx {
    ToolCtx {
        session: SessionId::new("ses_test"),
        agent: AgentId::new("agent_test"),
        agent_name: "test_agent".to_string(),
        call_id: CallId::new("call_test"),
        cwd,
        mode: Mode::Build,
        host: Arc::new(DummyHost::default()),
        cancel: CancellationToken::new(),
        output_cap_chars: 16_000,
        exec_yield_after: Duration::from_secs(10),
        exec_default_timeout: Duration::from_secs(30),
        tool_name: None,
        tool_kind: None,
    }
}

#[tokio::test]
async fn test_skill_tool_with_temp_dir() {
    let temp = TempDirGuard::new("tool_test");

    // Create a skill: "deploy"
    let deploy_dir = temp.path().join("deploy");
    std::fs::create_dir_all(&deploy_dir).expect("create deploy dir");
    std::fs::write(
        deploy_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy instructions\n---\n# How to deploy\nStep 1: build\nStep 2: run",
    )
    .expect("write deploy SKILL.md");

    // Load registry
    let (registry, warnings) = SkillRegistry::load(&[temp.path().to_path_buf()]);
    assert!(warnings.is_empty());
    assert_eq!(registry.skills().len(), 1);

    let tool = SkillTool::new(Arc::new(registry), 16384);
    let ctx = make_ctx(temp.path().to_path_buf());

    // 1. Successful skill loading
    let out = tool
        .call(json!({"name": "deploy"}), &ctx)
        .await
        .expect("call deploy tool");
    assert!(out.content.contains("# How to deploy"));
    assert!(out.content.contains("Step 1: build"));
    assert_eq!(out.title, "Skill deploy");

    // 2. Unknown skill error lists available skills
    let err = tool
        .call(json!({"name": "unknown_skill"}), &ctx)
        .await
        .expect_err("unknown skill should fail");
    let err_msg = err.to_string();
    assert!(err_msg.contains("unknown skill 'unknown_skill'"));
    assert!(err_msg.contains("deploy"));

    // 3. Empty name returns invalid argument error
    let err_empty = tool
        .call(json!({"name": "   "}), &ctx)
        .await
        .expect_err("empty name should fail");
    assert!(err_empty.to_string().contains("cannot be empty"));
}
