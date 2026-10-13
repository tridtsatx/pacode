//! Fixture builders shared by the snapshot tests and the `#[ignore]`d
//! gallery. Public API only, so the pair can be dropped into an older
//! checkout to render "before" shots. Each test crate uses a subset.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Instant;

use pacode_client::{ClientEvent, ClientEventContext};
use pacode_tui::layout::ScreenLayout;
use pacode_tui::state::AppState;
use pacode_tui::ui;
use pacode_types::ids::{AgentId, SessionId, TaskId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{
    AgentKind, AgentStatus, Mode, PlanStatus, ProgressSource, TaskProgress, TaskStatus,
};
use pacode_types::transcript::ToolStatus;
use pacode_types::{
    AgentInfo, Config, Plan, PlanItem, SessionMeta, SessionSnapshot, TaskInfo, TranscriptItem,
    TranscriptKind, UsageTotals,
};
use ratatui::Frame;

pub fn make_task(
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

pub fn make_base_snapshot() -> SessionSnapshot {
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
        zdr: false,
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
        pending_questions: Vec::new(),
        cron_jobs: Vec::new(),
        monitors: Vec::new(),
        seq: 10,
        meta,
        transcript,
        has_more_history: false,
        turn_active: true,
        active_turn: None,
        terminal_turn: None,
        terminal_error: None,
        plan,
        agents,
        tasks,
        usage,
        pending_permissions: vec![],
    }
}

pub fn create_state(cols: u16, rows: u16, snapshot: SessionSnapshot) -> AppState {
    let mut config = Config::default();
    config.ui.hints.effort = true;
    config.ui.hints.model = true;
    // Row 1 of the footer pads its segments only where the theme has status
    // shades; pin the colour mode so the text does not follow COLORTERM.
    config.ui.color = "truecolor".into();
    let mut state = AppState::new(config, "0.1.0-dev".into(), cols, rows);
    // The mascot is random per run; pin it so snapshots stay deterministic.
    state.mascot = pacode_tui::ui::mascot::MascotKind::Pacman;
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: ClientEventContext::unscoped(),
            snapshot,
        },
        now,
    );
    state.turn_started_at = None;
    // The phrase of the day rotates daily; pin it so snapshots stay deterministic.
    if let Some(header) = state.transcript.header.as_mut() {
        header.day = 0;
    }
    // The welcome cascade plays for the first 400 ms; pin the start back so
    // the header is fully arrived and snapshots stay deterministic.
    state.started_at = Instant::now() - std::time::Duration::from_secs(60);
    // The event loop refreshes the activity phase before every draw; these tests
    // drive `ui::draw` directly, so they have to do the same.
    state.tick_phase(now);
    state
}

/// Tests that set `focus` directly skip the event loop, so the first draw would
/// record a fresh `overlay_since` and the overlay would render mid-unfold.
/// Marking the layer already seen keeps the snapshot at full size.
pub fn settle_focus(state: &mut AppState) {
    state.last_focus_key = pacode_tui::ui::focus_key(&state.focus);
}

pub fn prepare_and_draw_frame(frame: &mut Frame<'_>, state: &mut AppState) -> ScreenLayout {
    let prepared = ui::prepare_frame(frame.area(), state, Instant::now());
    ui::draw(frame, state, &prepared)
}
