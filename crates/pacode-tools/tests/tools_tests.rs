use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::{TaskManager, TaskSpec};
use pacode_tools::Tool;
use pacode_tools::builtin::agent::AgentTool;
use pacode_tools::builtin::bash::BashTool;
use pacode_tools::builtin::bg::BgTool;
use pacode_tools::builtin::edit::EditTool;
use pacode_tools::builtin::glob::GlobTool;
use pacode_tools::builtin::grep::GrepTool;
use pacode_tools::builtin::ls::LsTool;
use pacode_tools::builtin::multi_edit::MultiEditTool;
use pacode_tools::builtin::plan::PlanTool;
use pacode_tools::builtin::read::ReadTool;
use pacode_tools::builtin::write::WriteTool;
use pacode_tools::host::{AgentSpec, PermissionDraft, ToolCtx, ToolHost, WaitOutcome};
use pacode_tools::output::ToolError;
use pacode_types::{
    AgentId, AgentInfo, AgentStatus, CallId, DiffStat, Effort, ExecConfig, Mode, ModelRoute,
    PermissionDecision, Plan, RiskLevel, SessionId, TaskId, TaskInfo, TaskProgress,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

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
        self.drafts.lock().unwrap().push(draft.clone());
        if draft.risk == Some(RiskLevel::Catastrophic) {
            return PermissionDecision::Deny;
        }
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

    async fn spawn_agent(&self, spec: AgentSpec) -> Result<AgentId, ToolError> {
        let id = AgentId::generate();
        let name = spec.name.unwrap_or_else(|| id.to_string());
        let info = AgentInfo {
            id: id.clone(),
            name,
            kind: pacode_types::AgentKind::Sub,
            status: AgentStatus::Thinking,
            activity: None,
            started_at_ms: pacode_types::now_ms(),
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 42,
            model: spec
                .model
                .unwrap_or_else(|| ModelRoute::parse_lossy("mock/test").unwrap()),
            effort: spec.effort.unwrap_or(Effort::Medium),
            parent: None,
            summary: None,
            error: None,
        };
        self.agents.lock().unwrap().push(info);
        Ok(id)
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

    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError> {
        if let Some(a) = self
            .agents
            .lock()
            .unwrap()
            .iter_mut()
            .find(|a| a.id == *agent)
        {
            a.status = AgentStatus::Stopped;
        }
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

#[tokio::test]
async fn test_read_offset_limit_and_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let text_file = tmp.path().join("test.txt");
    let mut text = String::new();
    for i in 1..=10 {
        text.push_str(&format!("line {i}\n"));
    }
    tokio::fs::write(&text_file, text.as_bytes()).await.unwrap();

    let tool = ReadTool;
    let out = tool
        .call(json!({"path": "test.txt", "offset": 3, "limit": 4}), &ctx)
        .await
        .unwrap();

    assert!(out.content.contains("     3\tline 3"));
    assert!(out.content.contains("     6\tline 6"));
    assert!(!out.content.contains("line 2"));
    assert!(!out.content.contains("line 7"));
    assert_eq!(out.preview, "4 lines");

    let bin_file = tmp.path().join("test.bin");
    tokio::fs::write(&bin_file, b"abc\0def").await.unwrap();
    let err = tool.call(json!({"path": "test.bin"}), &ctx).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn test_write_creates_dirs_and_asks_permission() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let tool = WriteTool;
    let out = tool
        .call(
            json!({"path": "sub/dir/new.txt", "content": "hello\nworld\n"}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(tmp.path().join("sub/dir/new.txt").exists());
    assert_eq!(
        out.diff,
        Some(DiffStat {
            added: 2,
            removed: 0
        })
    );

    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].title, "Write sub/dir/new.txt");
    assert!(drafts[0].detail.contains("new file, 2 lines"));

    // Overwrite
    let out2 = tool
        .call(
            json!({"path": "sub/dir/new.txt", "content": "hello\nzed\n"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        out2.diff,
        Some(DiffStat {
            added: 1,
            removed: 1
        })
    );
    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 2);
    assert!(drafts[1].detail.contains("-world"));
    assert!(drafts[1].detail.contains("+zed"));
}

#[tokio::test]
async fn test_edit_unique_ambiguous_replace_all_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let file_path = tmp.path().join("file.txt");
    tokio::fs::write(&file_path, "hello world\nhello zed\n")
        .await
        .unwrap();

    let tool = EditTool;

    // Not found
    let err_nf = tool
        .call(
            json!({"path": "file.txt", "old_string": "missing", "new_string": "x"}),
            &ctx,
        )
        .await;
    assert!(matches!(err_nf, Err(ToolError::InvalidInput(_))));

    // Ambiguous
    let err_amb = tool
        .call(
            json!({"path": "file.txt", "old_string": "hello", "new_string": "x"}),
            &ctx,
        )
        .await;
    assert!(matches!(err_amb, Err(ToolError::InvalidInput(_))));

    // Unique match
    let out = tool
        .call(
            json!({"path": "file.txt", "old_string": "hello world", "new_string": "hi world"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(out.content, "Edited file.txt: 1 replacement(s)");
    let content = tokio::fs::read_to_string(&file_path).await.unwrap();
    assert_eq!(content, "hi world\nhello zed\n");
    assert!(!tmp.path().join("file.txt.pacode-tmp").exists());

    // Replace all
    let out_all = tool
        .call(
            json!({"path": "file.txt", "old_string": "world", "new_string": "earth", "replace_all": true}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(out_all.content, "Edited file.txt: 1 replacement(s)");
}

#[tokio::test]
async fn test_multi_edit_all_or_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let file_path = tmp.path().join("multi.txt");
    tokio::fs::write(&file_path, "aaa\nbbb\nccc\n")
        .await
        .unwrap();

    let tool = MultiEditTool;

    // One edit fails -> nothing written
    let err = tool
        .call(
            json!({
                "path": "multi.txt",
                "edits": [
                    {"old_string": "aaa", "new_string": "AAA"},
                    {"old_string": "missing", "new_string": "ZZZ"}
                ]
            }),
            &ctx,
        )
        .await;
    assert!(matches!(err, Err(ToolError::InvalidInput(_))));
    let untouched = tokio::fs::read_to_string(&file_path).await.unwrap();
    assert_eq!(untouched, "aaa\nbbb\nccc\n");

    // All succeed
    let out = tool
        .call(
            json!({
                "path": "multi.txt",
                "edits": [
                    {"old_string": "aaa", "new_string": "AAA"},
                    {"old_string": "bbb", "new_string": "BBB"}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.contains("Edit #1: 1 replacement(s)"));
    assert!(out.content.contains("Edit #2: 1 replacement(s)"));
    let modified = tokio::fs::read_to_string(&file_path).await.unwrap();
    assert_eq!(modified, "AAA\nBBB\nccc\n");
}

#[tokio::test]
async fn test_bash_foreground_exit_code_and_output() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let tool = BashTool;

    let out_ok = tool
        .call(
            json!({"command": "echo 'hello foreground' && exit 0"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out_ok.content.contains("hello foreground"));
    assert!(out_ok.content.ends_with("\n[exit code 0]"));
    assert!(!out_ok.is_error);

    let out_fail = tool
        .call(
            json!({"command": "echo 'failing foreground' && exit 42"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out_fail.content.contains("failing foreground"));
    assert!(out_fail.content.ends_with("\n[exit code 42]"));
    assert!(out_fail.is_error);
}

#[tokio::test]
async fn test_bash_auto_yield() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let mut ctx = make_ctx(tmp.path().to_path_buf(), host.clone());
    ctx.exec_yield_after = Duration::from_millis(200);

    let tool = BashTool;
    let out = tool
        .call(json!({"command": "sleep 3"}), &ctx)
        .await
        .unwrap();
    assert!(out.task.is_some());
    assert!(out.content.contains("background"));
    assert!(out.content.contains("Command still running"));
}

#[tokio::test]
async fn test_bash_background_immediate() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let tool = BashTool;
    let start = std::time::Instant::now();
    let out = tool
        .call(json!({"command": "sleep 5", "background": true}), &ctx)
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(out.task.is_some());
    assert!(out.content.contains("background"));
}

#[tokio::test]
async fn test_bash_plan_mode_denies_touch_allows_ls() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let mut ctx = make_ctx(tmp.path().to_path_buf(), host.clone());
    ctx.mode = Mode::Plan;

    let tool = BashTool;

    let err = tool.call(json!({"command": "touch x"}), &ctx).await;
    assert!(matches!(err, Err(ToolError::Denied(_))));
    assert_eq!(host.drafts.lock().unwrap().len(), 0);

    let ok = tool.call(json!({"command": "ls"}), &ctx).await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn test_bash_catastrophic_denial() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let mut ctx = make_ctx(tmp.path().to_path_buf(), host.clone());
    ctx.mode = Mode::Bypass;

    let tool = BashTool;
    let err = tool.call(json!({"command": "rm -rf ~"}), &ctx).await;
    assert!(matches!(err, Err(ToolError::Denied(_))));

    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].risk, Some(RiskLevel::Catastrophic));
}

#[tokio::test]
async fn test_grep_glob_ls_with_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let host = Arc::new(StubHost::new(root.join("spool")));
    let ctx = make_ctx(root.to_path_buf(), host);

    tokio::fs::write(root.join(".gitignore"), b"ignored_dir/\n*.ignored\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("file1.rs"), b"fn hello_world() {}\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("file2.rs"), b"fn other() {}\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("test.ignored"), b"fn hello_ignored() {}\n")
        .await
        .unwrap();
    tokio::fs::write(root.join(".hidden_file"), b"fn hello_hidden() {}\n")
        .await
        .unwrap();

    let ign_dir = root.join("ignored_dir");
    tokio::fs::create_dir_all(&ign_dir).await.unwrap();
    tokio::fs::write(ign_dir.join("sub.rs"), b"fn hello_sub() {}\n")
        .await
        .unwrap();

    // Grep
    let grep = GrepTool;
    let out_grep = grep.call(json!({"pattern": "hello"}), &ctx).await.unwrap();
    assert!(out_grep.content.contains("file1.rs"));
    assert!(!out_grep.content.contains("test.ignored"));
    assert!(!out_grep.content.contains("ignored_dir"));
    assert!(!out_grep.content.contains(".hidden_file"));

    // Glob
    let glob = GlobTool;
    let out_glob = glob.call(json!({"pattern": "*.rs"}), &ctx).await.unwrap();
    assert!(out_glob.content.contains("file1.rs"));
    assert!(out_glob.content.contains("file2.rs"));
    assert!(!out_glob.content.contains("ignored_dir"));

    // Ls
    let ls = LsTool;
    let out_ls = ls.call(json!({}), &ctx).await.unwrap();
    assert!(out_ls.content.contains("file1.rs"));
    assert!(out_ls.content.contains("file2.rs"));
    assert!(!out_ls.content.contains("test.ignored"));
    assert!(!out_ls.content.contains("ignored_dir"));
    assert!(!out_ls.content.contains(".hidden_file"));
}

#[tokio::test]
async fn test_plan_set_and_update() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let tool = PlanTool;
    let set_out = tool
        .call(
            json!({
                "action": "set",
                "items": [
                    {"content": "Step 1", "status": "done"},
                    {"content": "Step 2", "status": "active", "progress": 60},
                    {"content": "Step 3", "status": "pending"}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(set_out.content.contains("[x] p1: Step 1"));
    assert!(set_out.content.contains("[*] (60%) p2: Step 2"));
    assert!(set_out.content.contains("[ ] p3: Step 3"));

    let update_out = tool
        .call(
            json!({
                "action": "update",
                "item_id": "p2",
                "status": "done"
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(update_out.content.contains("[x] p2: Step 2"));
    assert!(!update_out.content.contains("(60%)"));
}

#[tokio::test]
async fn test_agent_spawn_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let tool = AgentTool;
    let spawn_out = tool
        .call(
            json!({
                "action": "spawn",
                "prompt": "Investigate bug",
                "name": "worker",
                "model": "openai/gpt-4o",
                "effort": "high"
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(spawn_out.content.contains("Spawned agent"));
    assert!(spawn_out.content.contains("worker"));

    let list_out = tool.call(json!({"action": "list"}), &ctx).await.unwrap();
    assert!(list_out.content.contains("worker"));
    assert!(list_out.content.contains("thinking"));
    assert!(list_out.content.contains("↓42"));
}

#[test]
fn test_definition_ensures_common_properties() {
    let read = ReadTool;
    let def = read.definition();
    assert_eq!(def.name, "read");
    let props = &def.input_schema["properties"];
    assert!(props["intent"].is_object());
    assert!(props["accept_large_output"].is_object());
    let req = def.input_schema["required"].as_array().unwrap();
    assert!(req.iter().any(|v| v.as_str() == Some("intent")));
}

#[tokio::test]
async fn test_bg_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host.clone());

    let bash = BashTool;
    let spawn_res = bash
        .call(
            json!({"command": "echo 'bg line 1' && sleep 10", "background": true}),
            &ctx,
        )
        .await
        .unwrap();
    let task_id = spawn_res.task.unwrap();

    let bg = BgTool;
    let list_out = bg.call(json!({"action": "list"}), &ctx).await.unwrap();
    assert!(list_out.content.contains(task_id.as_str()));
    assert!(list_out.content.contains("running"));

    let prog_out = bg
        .call(
            json!({
                "action": "progress",
                "task_id": task_id.as_str(),
                "percent": 50.0
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(prog_out.content.contains("Reported progress"));

    let status_out = bg
        .call(
            json!({
                "action": "status",
                "task_id": task_id.as_str()
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(status_out.content.contains("Progress: 50%"));

    let kill_out = bg
        .call(
            json!({
                "action": "kill",
                "task_id": task_id.as_str()
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(kill_out.content.contains("killed"));
}

#[tokio::test]
async fn test_outside_workspace_detection() {
    let ws_tmp = tempfile::tempdir().unwrap();
    let outside_tmp = tempfile::tempdir().unwrap();

    let host = Arc::new(StubHost::new(ws_tmp.path().join("spool")));
    let ctx = make_ctx(ws_tmp.path().to_path_buf(), host.clone());

    let outside_file = outside_tmp.path().join("outside.txt");
    let outside_path_str = outside_file.to_string_lossy().to_string();

    // Write outside workspace
    let write_tool = WriteTool;
    let write_out = write_tool
        .call(
            json!({"path": outside_path_str, "content": "hello outside\n"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(write_out.content.contains("Wrote"));

    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1);
    assert!(
        drafts[0].title.contains("outside workspace"),
        "expected 'outside workspace' in title: {}",
        drafts[0].title
    );

    // Edit outside workspace
    let edit_tool = EditTool;
    let edit_out = edit_tool
        .call(
            json!({"path": outside_path_str, "old_string": "outside", "new_string": "world"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(edit_out.content.contains("Edited"));

    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 2);
    assert!(
        drafts[1].title.contains("outside workspace"),
        "expected 'outside workspace' in title: {}",
        drafts[1].title
    );

    // Multi-edit outside workspace
    let multi_edit_tool = MultiEditTool;
    let multi_out = multi_edit_tool
        .call(
            json!({
                "path": outside_path_str,
                "edits": [{"old_string": "world", "new_string": "earth"}]
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(multi_out.content.contains("Multi-edited"));

    let drafts = host.drafts.lock().unwrap().clone();
    assert_eq!(drafts.len(), 3);
    assert!(
        drafts[2].title.contains("outside workspace"),
        "expected 'outside workspace' in title: {}",
        drafts[2].title
    );
}

#[tokio::test]
async fn test_webfetch_url_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let tool = pacode_tools::builtin::webfetch::WebFetchTool;

    let err_scheme = tool
        .call(json!({"url": "ftp://example.com/file"}), &ctx)
        .await;
    assert!(matches!(err_scheme, Err(ToolError::InvalidInput(_))));

    let err_invalid = tool.call(json!({"url": "not a valid url"}), &ctx).await;
    assert!(matches!(err_invalid, Err(ToolError::InvalidInput(_))));
}

#[tokio::test]
async fn test_mcp_tools_proxy() {
    use pacode_mcp::McpPool;
    use pacode_types::McpServerConfig;
    use std::collections::BTreeMap;

    let python_ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !python_ok {
        return;
    }

    let fake_mcp_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("pacode-mcp/tests/fake_mcp.py");
    if !fake_mcp_path.exists() {
        return;
    }

    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![fake_mcp_path.to_string_lossy().to_string()],
        env: BTreeMap::new(),
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_string(), cfg);
    let pool = McpPool::new(servers, None, None);

    let tools = pacode_tools::builtin::mcp::mcp_tools(pool.clone()).await;
    assert_eq!(tools.len(), 3);

    let echo_tool = tools.iter().find(|t| t.name() == "fake__echo").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let out = echo_tool
        .call(
            json!({
                "msg": "hello from mcp",
                "intent": "testing",
                "accept_large_output": true
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("hello from mcp"));
    assert_eq!(out.title, "fake__echo");

    pool.shutdown().await;
}

#[tokio::test]
async fn test_mcp_resource_proxy() {
    use pacode_mcp::McpPool;
    use pacode_types::McpServerConfig;
    use std::collections::BTreeMap;

    let python_ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !python_ok {
        return;
    }

    let fake_mcp_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("pacode-mcp/tests/fake_mcp.py");
    if !fake_mcp_path.exists() {
        return;
    }

    let mut env = BTreeMap::new();
    env.insert("FAKE_MCP_RESOURCES".to_string(), "1".to_string());

    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![fake_mcp_path.to_string_lossy().to_string()],
        env,
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_string(), cfg);
    let pool = McpPool::new(servers, None, None);

    let tools = pacode_tools::builtin::mcp::mcp_tools(pool.clone()).await;
    // 3 tools (echo, fail, slow) + 1 resource proxy (fake__resource)
    assert_eq!(tools.len(), 4);

    let resource_tool = tools.iter().find(|t| t.name() == "fake__resource").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(StubHost::new(tmp.path().join("spool")));
    let ctx = make_ctx(tmp.path().to_path_buf(), host);

    let out = resource_tool
        .call(
            json!({
                "uri": "fake://resource1",
                "intent": "reading resource",
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.content.contains("content of resource1"));
    assert_eq!(out.title, "fake__resource");

    let out_blob = resource_tool
        .call(
            json!({
                "uri": "fake://resource2",
                "intent": "reading blob resource",
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out_blob.content.contains("[blob: image/png]"));

    pool.shutdown().await;
}
