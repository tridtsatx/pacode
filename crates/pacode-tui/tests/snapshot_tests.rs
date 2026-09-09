use std::path::PathBuf;
use std::time::Instant;

use pacode_client::ClientEvent;
use pacode_tui::layout::ScreenLayout;
use pacode_tui::state::{AppState, Focus, PanelTarget};
use pacode_tui::ui;
use pacode_types::ids::{AgentId, SessionId, TaskId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{
    AgentKind, AgentStatus, Mode, PlanStatus, ProgressSource, TaskProgress, TaskStatus,
};
use pacode_types::transcript::{DiffStat, ToolStatus};
use pacode_types::{
    AgentInfo, Config, Plan, PlanItem, SessionMeta, SessionSnapshot, TaskInfo, TranscriptItem,
    TranscriptKind, UsageTotals,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn make_task(
    id: &str,
    label: &str,
    status: TaskStatus,
    progress: Option<TaskProgress>,
    started: u64,
    ended: Option<u64>,
    errors: u32,
) -> TaskInfo {
    TaskInfo {
        id: TaskId::new(id),
        session: SessionId::new("ses_abcdef123"),
        owner: AgentId::main(),
        label: label.into(),
        command: label.into(),
        cwd: PathBuf::from("/home/user/project"),
        status,
        backgrounded: true,
        exit_code: if status == TaskStatus::Failed {
            Some(1)
        } else {
            Some(0)
        },
        started_at_ms: started,
        ended_at_ms: ended,
        progress,
        warnings: 0,
        errors,
        output_path: PathBuf::from("/tmp/task.log"),
        output_bytes: 1024,
        acked: false,
    }
}

fn make_base_snapshot() -> SessionSnapshot {
    let now = pacode_types::time::now_ms();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".into());
    let cwd = PathBuf::from(home).join("zed/pacode");

    let meta = SessionMeta {
        id: SessionId::new("ses_abcdef123"),
        name: Some("Refactor TUI layout".into()),
        cwd,
        git_branch: Some("master".into()),
        created_at_ms: now - 120_000,
        updated_at_ms: now - 1000,
        model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: None,
    };

    let plan = Plan {
        version: 1,
        items: vec![
            PlanItem {
                id: "1".into(),
                content: "Read codebase & analyze architecture".into(),
                status: PlanStatus::Done,
                progress: None,
            },
            PlanItem {
                id: "2".into(),
                content: "Implement TUI layout & widgets".into(),
                status: PlanStatus::Active,
                progress: Some(60),
            },
            PlanItem {
                id: "3".into(),
                content: "Add key bindings & event loop".into(),
                status: PlanStatus::Pending,
                progress: None,
            },
            PlanItem {
                id: "4".into(),
                content: "Write unit tests & snapshot tests".into(),
                status: PlanStatus::Pending,
                progress: None,
            },
        ],
    };

    let agents = vec![
        AgentInfo {
            id: AgentId::main(),
            name: "main".into(),
            kind: AgentKind::Main,
            status: AgentStatus::Thinking,
            activity: Some("thinking".into()),
            started_at_ms: now - 400,
            finished_at_ms: Some(now),
            tokens_in: 5400,
            tokens_out: 1200,
            model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
            effort: Effort::High,
            parent: None,
            summary: None,
            error: None,
        },
        AgentInfo {
            id: AgentId::new("agt_1"),
            name: "indexer".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::Finished,
            activity: Some("scanned 142 files".into()),
            started_at_ms: now - 2000,
            finished_at_ms: Some(now - 1600),
            tokens_in: 12000,
            tokens_out: 340,
            model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
            effort: Effort::Low,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
        AgentInfo {
            id: AgentId::new("agt_2"),
            name: "parser".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::Finished,
            activity: Some("parsed syntax trees".into()),
            started_at_ms: now - 1800,
            finished_at_ms: Some(now - 1400),
            tokens_in: 8500,
            tokens_out: 512,
            model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
            effort: Effort::Low,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
        AgentInfo {
            id: AgentId::new("agt_3"),
            name: "codegen".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::RunningTool,
            activity: Some("writing ui/rail.rs".into()),
            started_at_ms: now - 107_000,
            finished_at_ms: None,
            tokens_in: 9100,
            tokens_out: 2400,
            model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
            effort: Effort::High,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
        AgentInfo {
            id: AgentId::new("agt_4"),
            name: "tester".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::RunningTool,
            activity: Some("cargo test -p pacode-tui".into()),
            started_at_ms: now - 23_000,
            finished_at_ms: None,
            tokens_in: 4200,
            tokens_out: 180,
            model: ModelRoute::new("anthropic", "claude-3-7-sonnet"),
            effort: Effort::Medium,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
    ];

    let prog = TaskProgress {
        current: Some(214),
        total: Some(380),
        percent: None,
        message: None,
        source: ProgressSource::Reported,
        updated_at_ms: now - 10_000,
    };
    let tasks = vec![make_task(
        "tsk_1",
        "cargo test --all",
        TaskStatus::Running,
        Some(prog),
        now - 12_000,
        None,
        0,
    )];

    let usage = UsageTotals {
        input: 214_000,
        output: 12_400,
        reasoning: 8_100,
        cache_read: 152_000,
        cache_write: 61_000,
        cost_usd: Some(1.84),
        turns: 18,
        context_tokens: 10_700,
        context_window: Some(1_000_000),
        started_at_ms: now - 120_000,
        last_activity_ms: now - 1000,
    };

    let transcript = vec![
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1000,
            kind: TranscriptKind::User {
                text: "Refactor TUI layout and split into modules".into(),
            },
        },
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 1010,
            kind: TranscriptKind::Assistant {
                text: "I'll help you organize the TUI crate with a persistent rail and separate dialog column.".into(),
                complete: true,
            },
        },
        TranscriptItem {
            seq: 3,
            agent: AgentId::main(),
            ts_ms: 1020,
            kind: TranscriptKind::ToolCall {
                call_id: "call_1".into(),
                name: "read_file".into(),
                title: "crates/pacode-tui/src/layout.rs".into(),
                intent: None,
                status: ToolStatus::Ok,
                preview: "pub fn compute(area: Rect) -> ScreenLayout...".into(),
                diff: None,
                duration_ms: Some(120),
                task: None,
            },
        },
    ];

    SessionSnapshot {
        seq: 10,
        meta,
        transcript,
        has_more_history: false,
        turn_active: true,
        plan,
        agents,
        tasks,
        usage,
        pending_permissions: vec![],
    }
}

fn create_state(cols: u16, rows: u16, snapshot: SessionSnapshot) -> AppState {
    let mut config = Config::default();
    config.ui.hints.effort = true;
    config.ui.hints.model = true;
    let mut state = AppState::new(config, "0.1.0-dev".into(), cols, rows);
    let now = Instant::now();
    state.apply_client_event(ClientEvent::Snapshot(snapshot), now);
    state.turn_started_at = None;
    state
}

#[test]
fn test_mockup_state_01_120x34() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = ScreenLayout::default();
    terminal
        .draw(|f| {
            layout = ui::draw(f, &mut state);
        })
        .unwrap();

    // Assert that column rail_separator.x is │ on EVERY row
    assert!(layout.rail_separator.width > 0);
    let sep_x = layout.rail_separator.x;
    for y in 0..34 {
        let cell = terminal.backend().buffer().cell((sep_x, y)).unwrap();
        assert_eq!(
            cell.symbol(),
            "│",
            "Row {y} must have vertical separator '│' at column {sep_x}, but found '{}'",
            cell.symbol()
        );
    }

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_01_120x34", view);
}

#[test]
fn test_mockup_state_01_100x30() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(100, 30, snapshot);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = ScreenLayout::default();
    terminal
        .draw(|f| {
            layout = ui::draw(f, &mut state);
        })
        .unwrap();

    // Assert that column rail_separator.x is │ on EVERY row
    assert!(layout.rail_separator.width > 0);
    let sep_x = layout.rail_separator.x;
    for y in 0..30 {
        let cell = terminal.backend().buffer().cell((sep_x, y)).unwrap();
        assert_eq!(
            cell.symbol(),
            "│",
            "Row {y} must have vertical separator '│' at column {sep_x}, but found '{}'",
            cell.symbol()
        );
    }

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_01_100x30", view);
}

#[test]
fn test_mockup_state_01_80x24() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(80, 24, snapshot);

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_01_80x24", view);
}

#[test]
fn test_mockup_state_01_60x20() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(60, 20, snapshot);

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_01_60x20", view);
}

#[test]
fn test_mockup_state_02_select_agent() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);
    state.focus = Focus::SelectAgent { index: 3 };

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_02_120x34", view);
}

#[test]
fn test_mockup_state_03_panel() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);

    let target = PanelTarget::Agent(AgentId::new("agt_3"));
    state.focus = Focus::Panel {
        target: target.clone(),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(target);

    let panel_items = vec![
        TranscriptItem {
            seq: 101,
            agent: AgentId::new("agt_3"),
            ts_ms: 1310,
            kind: TranscriptKind::User {
                text: "Update rail.rs with new layout".into(),
            },
        },
        TranscriptItem {
            seq: 102,
            agent: AgentId::new("agt_3"),
            ts_ms: 1320,
            kind: TranscriptKind::Assistant {
                text: "Here is the diff for rail rendering.".into(),
                complete: true,
            },
        },
        TranscriptItem {
            seq: 103,
            agent: AgentId::new("agt_3"),
            ts_ms: 1330,
            kind: TranscriptKind::ToolCall {
                call_id: "call_diff".into(),
                name: "edit_file".into(),
                title: "crates/pacode-tui/src/ui/rail.rs".into(),
                intent: None,
                status: ToolStatus::Ok,
                preview: "--- a/rail.rs\n+++ b/rail.rs\n@@ -10,3 +10,5 @@\n+use ratatui::Frame;\n-old_code();\n+new_code();\n".into(),
                diff: Some(DiffStat { added: 64, removed: 12 }),
                duration_ms: Some(85),
                task: None,
            },
        },
    ];
    state.panel.agent_transcript.reset(panel_items, false);

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_03_120x34", view);
}

#[test]
fn test_mockup_state_05_bglist() {
    let mut snapshot = make_base_snapshot();
    let prog = TaskProgress {
        current: Some(214),
        total: Some(380),
        percent: None,
        message: None,
        source: ProgressSource::Reported,
        updated_at_ms: 1500,
    };
    snapshot.tasks = vec![
        make_task(
            "tsk_1",
            "cargo test --all",
            TaskStatus::Running,
            Some(prog),
            1500,
            None,
            0,
        ),
        make_task(
            "tsk_2",
            "cargo build --release",
            TaskStatus::Completed,
            None,
            1000,
            Some(183_000),
            0,
        ),
        make_task(
            "tsk_3",
            "npm run lint",
            TaskStatus::Failed,
            None,
            1100,
            Some(1200),
            3,
        ),
        make_task(
            "tsk_4",
            "tsc --noEmit",
            TaskStatus::Completed,
            None,
            1200,
            Some(13_200),
            0,
        ),
    ];

    let mut state = create_state(120, 34, snapshot);
    state.focus = Focus::BgList { index: 0 };

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_05_120x34", view);
}

#[test]
fn test_mockup_state_06_idle() {
    let mut snapshot = make_base_snapshot();
    let now = pacode_types::time::now_ms();
    snapshot.turn_active = false;
    for a in &mut snapshot.agents {
        a.status = AgentStatus::Finished;
        if a.id.is_main() {
            a.finished_at_ms = Some(now);
        }
    }
    snapshot.tasks.clear();

    let mut state = create_state(120, 34, snapshot);
    state.turn_active = false;
    state.rail.show_session_stats = true;

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_06_120x34", view);
}

#[test]
fn test_no_subagents_hides_agents_zone() {
    let mut snapshot = make_base_snapshot();
    // Keep only main agent (no subagents)
    snapshot.agents.retain(|a| a.id.is_main());
    let mut state = create_state(120, 34, snapshot);
    state.rail.show_session_stats = false;

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert!(!view.contains("AGENTS"));
    assert!(!view.contains("● main"));
}

#[test]
fn test_turn_ended_response_stats_rendered() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);

    let now = Instant::now();
    // Simulate TurnEnded with usage
    state.apply_event(
        100,
        pacode_types::Event::TurnEnded {
            agent: AgentId::main(),
            turn: pacode_types::TurnId::new("trn_test"),
            usage: Some(pacode_types::stream::Usage {
                input_tokens: 5400,
                output_tokens: 1200,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            stop: pacode_types::TurnStop::Completed,
        },
        now + std::time::Duration::from_millis(2400),
    );

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            ui::draw(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert!(
        view.contains("tok/s"),
        "Dialog view must render assistant response stats line with 'tok/s', but view was:\n{view}"
    );
    assert!(
        view.contains("↑1.2k ↓5.4k"),
        "Dialog view must render '↑1.2k ↓5.4k', but view was:\n{view}"
    );
}
