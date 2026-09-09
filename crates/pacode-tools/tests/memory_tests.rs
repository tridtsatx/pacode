use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::{TaskManager, TaskSpec};
use pacode_tools::Tool;
use pacode_tools::builtin::memory::{
    MemoryReadTool, MemoryWriteTool, global_memory_path, project_memory_path,
};
use pacode_tools::host::{AgentSpec, PermissionDraft, ToolCtx, ToolHost, WaitOutcome};
use pacode_tools::output::ToolError;
use pacode_types::{
    AgentId, AgentInfo, CallId, ExecConfig, Mode, PermissionDecision, Plan, SessionId, TaskId,
    TaskInfo, TaskProgress,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

static ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct StubHost {
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

    async fn wait_agent(&self, _agent: &AgentId, _timeout: Duration) -> WaitOutcome {
        WaitOutcome::Finished
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
    }
}

#[tokio::test]
async fn test_memory_write_and_read_project_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;
    let out = write_tool
        .call(
            json!({"scope": "project", "text": "use rust edition 2024"}),
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(out.content, "stored");

    // Parent dir .pacode must have been created and file written
    let project_file = project_memory_path(&ctx.cwd);
    assert!(project_file.exists());
    let text = tokio::fs::read_to_string(&project_file).await.unwrap();
    assert_eq!(text, "- use rust edition 2024\n");

    // Permission draft was requested for project write
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].title, "Write .pacode/memory.md");
    assert!(drafts[0].detail.contains("- use rust edition 2024"));

    // Read back project scope
    let read_tool = MemoryReadTool;
    let read_out = read_tool
        .call(json!({"scope": "project"}), &ctx)
        .await
        .unwrap();
    assert!(read_out.content.contains("- use rust edition 2024"));
}

#[tokio::test]
async fn test_memory_write_and_read_global_scope_no_permission_prompt() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let global_dir = tmp.path().join("home_pacode");
    // Set PACODE_HOME so global_memory_path points to global_dir/memory.md
    unsafe {
        std::env::set_var("PACODE_HOME", &global_dir);
    }

    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;
    let out = write_tool
        .call(
            json!({"scope": "global", "text": "prefer concise code"}),
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(out.content, "stored");

    let global_file = global_memory_path();
    assert!(global_file.exists());
    let text = tokio::fs::read_to_string(&global_file).await.unwrap();
    assert_eq!(text, "- prefer concise code\n");

    // Global scope is pacode-owned: allowed WITHOUT permission prompt!
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(
        drafts.len(),
        0,
        "global scope should not trigger a permission prompt"
    );

    // Read back global scope
    let read_tool = MemoryReadTool;
    let read_out = read_tool
        .call(json!({"scope": "global"}), &ctx)
        .await
        .unwrap();
    assert!(read_out.content.contains("- prefer concise code"));

    unsafe {
        std::env::remove_var("PACODE_HOME");
    }
}

#[tokio::test]
async fn test_memory_write_dedup() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;

    // First write: stored
    let out1 = write_tool
        .call(json!({"scope": "project", "text": "unique note"}), &ctx)
        .await
        .unwrap();
    assert_eq!(out1.content, "stored");

    // Exact duplicate: already stored
    let out2 = write_tool
        .call(json!({"scope": "project", "text": "unique note"}), &ctx)
        .await
        .unwrap();
    assert_eq!(out2.content, "already stored");

    // Whitespace variation that trims to same content: already stored
    let out3 = write_tool
        .call(
            json!({"scope": "project", "text": "  unique note  \n"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(out3.content, "already stored");

    // File only contains 1 line
    let project_file = project_memory_path(&ctx.cwd);
    let text = tokio::fs::read_to_string(&project_file).await.unwrap();
    assert_eq!(text, "- unique note\n");
}

#[tokio::test]
async fn test_memory_write_newline_collapse_and_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;

    // Newline collapse
    let multi_line_text = "first line\nsecond line\r\nthird line\n  fourth line  ";
    let out = write_tool
        .call(json!({"scope": "project", "text": multi_line_text}), &ctx)
        .await
        .unwrap();
    assert_eq!(out.content, "stored");

    let project_file = project_memory_path(&ctx.cwd);
    let text = tokio::fs::read_to_string(&project_file).await.unwrap();
    assert_eq!(text, "- first line second line third line fourth line\n");

    // Cap to 500 characters
    let very_long = "a".repeat(600);
    let out_cap = write_tool
        .call(json!({"scope": "project", "text": very_long}), &ctx)
        .await
        .unwrap();
    assert_eq!(out_cap.content, "stored");

    let text_after = tokio::fs::read_to_string(&project_file).await.unwrap();
    let lines: Vec<&str> = text_after.lines().collect();
    assert_eq!(lines.len(), 2);
    let expected_line = format!("- {}", "a".repeat(500));
    assert_eq!(lines[1], expected_line);
}

#[tokio::test]
async fn test_memory_read_both_scopes() {
    let _lock = ENV_MUTEX.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let global_dir = tmp.path().join("global_home");
    unsafe {
        std::env::set_var("PACODE_HOME", &global_dir);
    }

    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;
    write_tool
        .call(json!({"scope": "global", "text": "global knowledge"}), &ctx)
        .await
        .unwrap();
    write_tool
        .call(
            json!({"scope": "project", "text": "project knowledge"}),
            &ctx,
        )
        .await
        .unwrap();

    let read_tool = MemoryReadTool;
    let read_both = read_tool.call(json!({}), &ctx).await.unwrap();

    assert!(read_both.content.contains("# Memory (global)"));
    assert!(read_both.content.contains("- global knowledge"));
    assert!(read_both.content.contains("# Memory (project)"));
    assert!(read_both.content.contains("- project knowledge"));

    unsafe {
        std::env::remove_var("PACODE_HOME");
    }
}

#[tokio::test]
async fn test_memory_write_permission_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    *host.decision.lock().unwrap() = PermissionDecision::Deny;
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let write_tool = MemoryWriteTool;
    let err = write_tool
        .call(
            json!({"scope": "project", "text": "should be rejected"}),
            &ctx,
        )
        .await;

    assert!(matches!(err, Err(ToolError::Denied(_))));
    assert!(!project_memory_path(&ctx.cwd).exists());
}
