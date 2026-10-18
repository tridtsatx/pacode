use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::{TaskManager, TaskSpec};
use pacode_tools::Tool;
use pacode_tools::builtin::memory::{
    MemoryForgetTool, MemoryListTool, MemoryReadTool, MemoryRecallTool, MemoryWriteTool,
};
use pacode_tools::host::{
    AgentKindDef, AgentSpec, PermissionDraft, ToolCtx, ToolHost, WaitOutcome,
};
use pacode_tools::output::ToolError;
use pacode_types::{
    AgentId, AgentInfo, CallId, ExecConfig, Mode, ModelRoute, PermissionDecision, Plan, SessionId,
    TaskId, TaskInfo, TaskProgress,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

static ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct StubHost {
    schedule: pacode_tools::test_support::ScheduleStub,
    drafts: Mutex<Vec<PermissionDraft>>,
    decision: Mutex<PermissionDecision>,
    tasks: Arc<TaskManager>,
    plan: Mutex<Plan>,
    agents: Mutex<Vec<AgentInfo>>,
}

impl StubHost {
    fn new(spool_dir: PathBuf) -> Self {
        let exec_cfg = ExecConfig::default();
        let tasks = TaskManager::new(spool_dir, exec_cfg);
        Self {
            schedule: pacode_tools::test_support::ScheduleStub::default(),
            drafts: Mutex::new(Vec::new()),
            decision: Mutex::new(PermissionDecision::AllowOnce),
            tasks,
            plan: Mutex::new(Plan::default()),
            agents: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ToolHost for StubHost {
    async fn ask_question(
        &self,
        _call_id: &pacode_types::CallId,
        _header: String,
        _question: String,
        options: Vec<pacode_types::QuestionOption>,
        _multi_select: bool,
    ) -> Result<pacode_types::QuestionAnswer, ToolError> {
        // A stub user always takes the recommended option, or the first one.
        let index = options.iter().position(|o| o.recommended).unwrap_or(0);
        Ok(pacode_types::QuestionAnswer::choice(index))
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

    async fn request_permission(&self, draft: PermissionDraft) -> PermissionDecision {
        let dec = *self.decision.lock().unwrap();
        self.drafts.lock().unwrap().push(draft);
        dec
    }

    async fn spawn_task(&self, spec: TaskSpec) -> Result<TaskId, ToolError> {
        let info = self
            .tasks
            .spawn(spec)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;
        Ok(info.id)
    }

    fn task_info(&self, task: &TaskId) -> Option<TaskInfo> {
        self.tasks.info(task)
    }

    fn list_tasks(&self) -> Vec<TaskInfo> {
        self.tasks.list(None)
    }

    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome {
        let res = self.tasks.wait(task, timeout, return_on_progress).await;
        match res {
            pacode_exec::WaitResult::Ended(_) => WaitOutcome::Finished,
            pacode_exec::WaitResult::Progress(_) => WaitOutcome::Progress,
            pacode_exec::WaitResult::Timeout(_) => WaitOutcome::Timeout,
            pacode_exec::WaitResult::NotFound => WaitOutcome::Finished,
        }
    }

    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError> {
        self.tasks
            .kill(task)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))
    }

    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError> {
        self.tasks
            .tail(task, lines)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))
    }

    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError> {
        self.tasks
            .report_progress(task, progress)
            .map_err(|e| ToolError::failed(e.to_string()))
    }

    async fn spawn_agent(&self, _spec: AgentSpec) -> Result<AgentId, ToolError> {
        Ok(AgentId::generate())
    }

    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == *agent)
            .cloned()
    }

    fn list_agents(&self) -> Vec<AgentInfo> {
        self.agents.lock().unwrap().clone()
    }

    fn agent_kinds(&self) -> Vec<AgentKindDef> {
        Vec::new()
    }

    fn parse_model_route(&self, s: &str) -> Option<ModelRoute> {
        ModelRoute::parse_lossy(s)
    }

    fn request_agent_status(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }

    fn report_status(&self, _text: String) -> Result<(), ToolError> {
        Ok(())
    }

    async fn stop_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }

    fn plan(&self) -> Plan {
        self.plan.lock().unwrap().clone()
    }

    fn set_plan(&self, plan: Plan) {
        *self.plan.lock().unwrap() = plan;
    }

    fn emit_preview(&self, _call_id: &CallId, _preview: String) {}

    fn emit_notice(&self, _level: pacode_types::ToastLevel, _text: String) {}
}

fn make_ctx(cwd: PathBuf, host: Arc<dyn ToolHost>) -> ToolCtx {
    ToolCtx {
        session: SessionId::new("ses_test"),
        agent: AgentId::new("agent_test"),
        agent_name: "test_agent".to_string(),
        call_id: CallId::new("call_test"),
        cwd,
        mode: Mode::Build,
        host,
        cancel: CancellationToken::new(),
        output_cap_chars: 16_000,
        exec_yield_after: Duration::from_secs(10),
        exec_default_timeout: Duration::from_secs(30),
        tool_name: None,
        tool_kind: None,
    }
}

/// Point the global scope at a temp dir for the duration of a test.
fn set_global(dir: &std::path::Path) {
    // SAFETY: serialised by ENV_MUTEX in every test of this file.
    unsafe {
        std::env::set_var("PACODE_HOME", dir);
    }
}

fn clear_global() {
    // SAFETY: serialised by ENV_MUTEX in every test of this file.
    unsafe {
        std::env::remove_var("PACODE_HOME");
    }
}

fn id_of(out: &str) -> String {
    out.split_whitespace().nth(1).unwrap().to_string()
}

#[tokio::test]
async fn test_memory_write_and_read_project_scope() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    set_global(&tmp.path().join("global"));
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    // Old call shape: scope + text only.
    let out = MemoryWriteTool
        .call(
            json!({"scope": "project", "text": "use rust edition 2024"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.starts_with("stored "), "{}", out.content);

    assert!(ctx.cwd.join(".pacode").join("memory.db").exists());

    // Permission draft was requested for the project write.
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].title, "Write .pacode/memory.db");
    assert!(drafts[0].detail.contains("use rust edition 2024"));

    // Old alias reads it back.
    let read_out = MemoryReadTool
        .call(json!({"scope": "project"}), &ctx)
        .await
        .unwrap();
    assert!(read_out.content.contains("use rust edition 2024"));
    clear_global();
}

#[tokio::test]
async fn test_memory_global_scope_no_permission_prompt_and_recall() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let global_dir = tmp.path().join("home_pacode");
    set_global(&global_dir);
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let out = MemoryWriteTool
        .call(
            json!({"scope": "global", "kind": "preference", "title": "Code style",
                   "text": "prefer concise code", "tags": ["style"]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.starts_with("stored "));
    assert!(global_dir.join("memory.db").exists());
    assert_eq!(
        host.drafts.lock().unwrap().len(),
        0,
        "global writes never prompt"
    );

    let found = MemoryRecallTool
        .call(json!({"query": "concise", "scope": "global"}), &ctx)
        .await
        .unwrap();
    assert!(found.content.contains("[global/preference]"));
    assert!(found.content.contains("prefer concise code"));
    clear_global();
}

#[tokio::test]
async fn test_memory_dedupe_update_and_forget() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    set_global(&tmp.path().join("global"));
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let first = MemoryWriteTool
        .call(
            json!({"scope": "global", "title": "Unique", "text": "unique note"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(first.content.starts_with("stored "));
    let second = MemoryWriteTool
        .call(
            json!({"scope": "global", "title": "unique", "text": "unique note v2"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(second.content.starts_with("updated "));
    assert_eq!(id_of(&first.content), id_of(&second.content));

    let listed = MemoryListTool.call(json!({}), &ctx).await.unwrap();
    assert!(listed.content.starts_with("1 memories"));

    let id = id_of(&first.content);
    let f = MemoryForgetTool
        .call(json!({"id": id}), &ctx)
        .await
        .unwrap();
    assert!(f.content.starts_with("forgot "));
    let gone = MemoryRecallTool
        .call(json!({"query": "unique"}), &ctx)
        .await
        .unwrap();
    assert_eq!(gone.content, "no memories found");
    assert!(
        MemoryForgetTool
            .call(json!({"id": id}), &ctx)
            .await
            .is_err()
    );
    clear_global();
}

#[tokio::test]
async fn test_memory_recall_merges_both_scopes() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    set_global(&tmp.path().join("global_home"));
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    MemoryWriteTool
        .call(json!({"scope": "global", "text": "global knowledge"}), &ctx)
        .await
        .unwrap();
    MemoryWriteTool
        .call(
            json!({"scope": "project", "text": "project knowledge"}),
            &ctx,
        )
        .await
        .unwrap();

    let both = MemoryRecallTool
        .call(json!({"query": "knowledge"}), &ctx)
        .await
        .unwrap();
    assert!(both.content.contains("[global/fact]"));
    assert!(both.content.contains("[project/fact]"));
    clear_global();
}

#[tokio::test]
async fn test_memory_legacy_markdown_is_migrated() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let global_dir = tmp.path().join("global_home");
    std::fs::create_dir_all(&global_dir).unwrap();
    std::fs::write(
        global_dir.join("memory.md"),
        "- legacy bullet one\n- legacy bullet two\n",
    )
    .unwrap();
    set_global(&global_dir);
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let out = MemoryRecallTool
        .call(json!({"query": "legacy", "scope": "global"}), &ctx)
        .await
        .unwrap();
    assert!(out.content.contains("legacy bullet one"));
    assert!(out.content.contains("legacy bullet two"));
    assert!(!global_dir.join("memory.md").exists());
    assert!(global_dir.join("memory.md.migrated").exists());
    clear_global();
}

#[tokio::test]
async fn test_memory_write_permission_denied() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    set_global(&tmp.path().join("global"));
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    *host.decision.lock().unwrap() = PermissionDecision::Deny;
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let err = MemoryWriteTool
        .call(
            json!({"scope": "project", "text": "should be rejected"}),
            &ctx,
        )
        .await;

    assert!(matches!(err, Err(ToolError::Denied(_))));
    assert!(!ctx.cwd.join(".pacode").join("memory.db").exists());
    clear_global();
}
