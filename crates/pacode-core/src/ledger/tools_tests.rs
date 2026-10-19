//! End to end: the real `read`/`edit`/`write` tools against a `SessionHost`, two
//! agents (main + a subagent) sharing one working tree.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use pacode_tools::builtin::edit::EditTool;
use pacode_tools::builtin::read::ReadTool;
use pacode_tools::builtin::write::WriteTool;
use pacode_tools::hashline::line_hash;
use pacode_tools::{Tool, ToolCtx, ToolError, ToolKind};
use pacode_types::{AgentId, AgentInfo, CallId, Config, ModelRoute};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::compaction::CompactionPinned;
use crate::host::SessionHost;
use crate::session::Session;

fn info(id: &AgentId, name: &str, route: &ModelRoute) -> AgentInfo {
    AgentInfo {
        id: id.clone(),
        name: name.to_string(),
        kind: if id.is_main() {
            pacode_types::AgentKind::Main
        } else {
            pacode_types::AgentKind::Sub
        },
        status: pacode_types::AgentStatus::Idle,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: route.clone(),
        effort: pacode_types::Effort::Medium,
        parent: (!id.is_main()).then(AgentId::main),
        summary: None,
        error: None,
    }
}

fn sub_id() -> AgentId {
    AgentId::new("agt_reviewer")
}

/// A session in auto mode (edits need no prompt) with `main` and `reviewer`.
fn session() -> (tempfile::TempDir, Arc<Session>) {
    let temp = tempfile::tempdir().unwrap();
    let id = pacode_types::SessionId::generate();
    let route = ModelRoute {
        provider: "mock".into(),
        model: "mock-model".into(),
    };
    let meta = pacode_types::SessionMeta {
        id: id.clone(),
        name: None,
        cwd: temp.path().canonicalize().unwrap(),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: route.clone(),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Auto,
        first_prompt: None,
        zdr: false,
    };
    let tools = pacode_tools::ToolRegistry::new();
    let mut agents = BTreeMap::new();
    for (agent_id, name) in [(AgentId::main(), "main"), (sub_id(), "reviewer")] {
        let agent = Arc::new(Agent::new(
            agent_id.clone(),
            info(&agent_id, name, &route),
            crate::agent::History::default(),
            tools.clone(),
            200,
            None,
        ));
        agents.insert(agent_id, agent);
    }
    let session = Arc::new(Session {
        id,
        meta: RwLock::new(meta),
        agents: RwLock::new(agents),
        plan: RwLock::new(pacode_types::Plan::default()),
        usage: RwLock::new(pacode_types::UsageTotals::default()),
        permissions: crate::permissions::PermissionState::default(),
        questions: crate::questions::QuestionState::default(),
        events: crate::transcript::EventSink::new(),
        tasks: pacode_exec::TaskManager::new(
            temp.path().join(".spool"),
            pacode_types::ExecConfig::default(),
        ),
        store: pacode_store::Store::open_in_memory().unwrap(),
        config: Arc::new(Config::default()),
        providers: Arc::new(pacode_provider::ProviderRegistry::empty()),
        tools: RwLock::new(tools),
        mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        app_version: "test".to_string(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
        scheduler: crate::schedule::Scheduler::new(),
        ledger: crate::ledger::FileLedger::new(),
        efforts_cache: Default::default(),
        agent_aliases: std::sync::Mutex::new(Default::default()),
        accounting: Arc::new(crate::analyze::SessionAccounting::new()),
        submit_lock: tokio::sync::Mutex::new(()),
        closed: std::sync::atomic::AtomicBool::new(false),
        recovery_admissions: std::sync::Mutex::new(Default::default()),
        model_flags: RwLock::new(Default::default()),
        global_events: crate::transcript::EventSink::new(),
        idle_watch: crate::core::IdleWatch::default(),
    });
    (temp, session)
}

fn ctx(session: &Arc<Session>, agent: &AgentId, name: &str) -> ToolCtx {
    ToolCtx {
        session: session.id.clone(),
        agent: agent.clone(),
        agent_name: name.to_string(),
        call_id: CallId::new("call_ledger"),
        cwd: session.meta().cwd,
        mode: pacode_types::Mode::Auto,
        host: Arc::new(SessionHost {
            session: session.clone(),
            agent: agent.clone(),
        }),
        cancel: CancellationToken::new(),
        output_cap_chars: 16_000,
        exec_yield_after: Duration::from_secs(5),
        exec_default_timeout: Duration::from_secs(30),
        tool_name: None,
        tool_kind: None,
        tools: pacode_tools::ToolRegistry::default(),
    }
    .with_tool("edit", ToolKind::Edit)
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn anchor(content: &str, line: usize) -> String {
    let text = content.lines().nth(line - 1).unwrap_or("");
    format!("{line}#{}", line_hash(text))
}

async fn read(ctx: &ToolCtx) {
    ReadTool.call(json!({"path": "f.txt"}), ctx).await.unwrap();
}

async fn replace(ctx: &ToolCtx, seen: &str, line: usize, with: &str) -> Result<String, ToolError> {
    EditTool
        .call(
            json!({"path": "f.txt", "ops": [
                {"op": "replace", "start": anchor(seen, line), "lines": [with]}
            ]}),
            ctx,
        )
        .await
        .map(|out| out.content)
}

async fn on_disk(dir: &Path) -> String {
    tokio::fs::read_to_string(dir.join("f.txt")).await.unwrap()
}

#[tokio::test]
async fn edit_without_read_is_rejected_and_read_unlocks_it() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(5))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let seen = numbered(5);

    let err = replace(&main, &seen, 2, "two").await.unwrap_err();
    let ToolError::Failed(msg) = err else {
        panic!("expected a ledger rejection, got {err:?}");
    };
    assert!(msg.starts_with("read f.txt first"), "{msg}");
    assert_eq!(on_disk(dir.path()).await, seen, "nothing written");

    read(&main).await;
    replace(&main, &seen, 2, "two").await.unwrap();
    // Its own writes keep it in sync: no re-read between edits.
    let now = on_disk(dir.path()).await;
    replace(&main, &now, 3, "three").await.unwrap();
    let now = on_disk(dir.path()).await;
    replace(&main, &now, 2, "two again").await.unwrap();
}

#[tokio::test]
async fn subagent_hashline_edit_on_untouched_lines_passes_with_note() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(40))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let reviewer = ctx(&session, &sub_id(), "reviewer");
    let seen = numbered(40);
    read(&main).await;
    read(&reviewer).await;

    replace(&main, &seen, 3, "main's line").await.unwrap();
    let out = replace(&reviewer, &seen, 30, "reviewer's line")
        .await
        .unwrap();
    assert!(
        out.contains("Note: f.txt was also modified by agent main since you read it"),
        "{out}"
    );
    let now = on_disk(dir.path()).await;
    assert!(now.contains("main's line") && now.contains("reviewer's line"));
}

#[tokio::test]
async fn subagent_overwriting_main_change_is_rejected_naming_main() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(10))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let reviewer = ctx(&session, &sub_id(), "reviewer");
    let seen = numbered(10);
    read(&main).await;
    read(&reviewer).await;
    replace(&main, &seen, 5, "main's line").await.unwrap();

    // A full write from its stale copy would revert main's line.
    let err = WriteTool
        .call(json!({"path": "f.txt", "content": seen}), &reviewer)
        .await
        .unwrap_err();
    let ToolError::Failed(msg) = err else {
        panic!("expected a ledger rejection, got {err:?}");
    };
    assert!(
        msg.starts_with("f.txt was modified by agent main (1 lines changed at L5)"),
        "{msg}"
    );

    assert!(
        on_disk(dir.path()).await.contains("main's line"),
        "nothing written"
    );

    // The rejection showed line 5 with fresh anchors: one retry suffices.
    let now = on_disk(dir.path()).await;
    replace(&reviewer, &now, 5, "agreed line").await.unwrap();
    assert!(on_disk(dir.path()).await.contains("agreed line"));
}

#[tokio::test]
async fn old_string_missing_after_another_agent_edit_names_the_writer() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(10))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let reviewer = ctx(&session, &sub_id(), "reviewer");
    let seen = numbered(10);
    read(&main).await;
    read(&reviewer).await;
    replace(&main, &seen, 5, "main's line").await.unwrap();

    // The reviewer's old_string spans line 5, which main has changed.
    let err = EditTool
        .call(
            json!({"path": "f.txt", "old_string": "line 4\nline 5\nline 6\n",
                   "new_string": "block\n"}),
            &reviewer,
        )
        .await
        .unwrap_err();
    let ToolError::InvalidInput(msg) = err else {
        panic!("expected InvalidInput, got {err:?}");
    };
    assert!(msg.starts_with("f.txt was modified by agent main"), "{msg}");
    assert!(msg.contains("old_string not found"), "{msg}");
    assert!(on_disk(dir.path()).await.contains("main's line"));
}

#[tokio::test]
async fn stale_anchors_after_another_agent_edit_name_the_writer() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(10))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let reviewer = ctx(&session, &sub_id(), "reviewer");
    let seen = numbered(10);
    read(&main).await;
    read(&reviewer).await;
    replace(&main, &seen, 6, "main's line").await.unwrap();

    let err = replace(&reviewer, &seen, 6, "reviewer's line")
        .await
        .unwrap_err();
    let ToolError::InvalidInput(msg) = err else {
        panic!("expected InvalidInput, got {err:?}");
    };
    assert!(msg.starts_with("f.txt was modified by agent main"), "{msg}");
    assert!(msg.contains("stale anchor"), "hashline detail kept: {msg}");
}

#[tokio::test]
async fn new_file_by_write_needs_no_read() {
    let (dir, session) = session();
    let main = ctx(&session, &AgentId::main(), "main");
    WriteTool
        .call(json!({"path": "f.txt", "content": "a\nb\n"}), &main)
        .await
        .unwrap();
    let now = on_disk(dir.path()).await;
    replace(&main, &now, 1, "A").await.unwrap();
    assert_eq!(on_disk(dir.path()).await, "A\nb\n");
}

#[tokio::test]
async fn bash_hook_refresh_and_compaction_digest() {
    let (dir, session) = session();
    tokio::fs::write(dir.path().join("f.txt"), numbered(10))
        .await
        .unwrap();
    let main = ctx(&session, &AgentId::main(), "main");
    let reviewer = ctx(&session, &sub_id(), "reviewer");
    let seen = numbered(10);
    read(&main).await;
    read(&reviewer).await;

    // Main's bash command rewrites line 8; the post-tool hook refreshes the ledger.
    let changed = seen.replace("line 8\n", "line eight, rewritten by sed\n");
    tokio::fs::write(dir.path().join("f.txt"), &changed)
        .await
        .unwrap();
    let main_agent = session.main_agent().unwrap();
    crate::hooks::post_tool_use(&session, &main_agent, "bash", &json!({}), &json!({})).await;

    let digest = session.pinned_blocks_for(&sub_id()).join("\n");
    assert!(digest.contains("f.txt"), "{digest}");
    assert!(digest.contains("MODIFIED by agent main"), "{digest}");
    let mine = session.pinned_blocks_for(&AgentId::main()).join("\n");
    assert!(
        mine.contains("f.txt") && !mine.contains("MODIFIED"),
        "{mine}"
    );

    let err = replace(&reviewer, &changed, 8, "reviewer")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("modified by agent main"), "{err}");
    replace(&main, &changed, 8, "main").await.unwrap();
}
