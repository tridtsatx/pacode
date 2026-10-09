mod common;

use std::time::Instant;

use common::*;
use pacode_client::{ClientEvent, ClientEventContext};
use pacode_tui::layout::ScreenLayout;
use pacode_tui::state::{AppState, CellKind, Focus, PanelTarget, PromptBatch};
use pacode_types::ids::{AgentId, SessionId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentStatus, ProgressSource, TaskProgress, TaskStatus};
use pacode_types::transcript::{DiffStat, ToolStatus};
use pacode_types::{Attach, Config, Request, TranscriptItem, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn test_mockup_state_01_120x34() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut layout = ScreenLayout::default();
    terminal
        .draw(|f| {
            layout = prepare_and_draw_frame(f, &mut state);
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
            layout = prepare_and_draw_frame(f, &mut state);
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
            prepare_and_draw_frame(f, &mut state);
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
            prepare_and_draw_frame(f, &mut state);
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
    settle_focus(&mut state);

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_02_120x34", view);
}

#[test]
fn test_mockup_state_03_panel() {
    let snapshot = make_base_snapshot();
    let mut state = create_state(120, 34, snapshot);
    // This mockup is the split view; the replace view has its own snapshot below.
    state.config.ui.agent_view = pacode_types::config::AgentView::Panel;

    let target = PanelTarget::Agent(AgentId::new("agt_3"));
    state.focus = Focus::Panel {
        target: target.clone(),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(target);
    settle_focus(&mut state);

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
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    insta::assert_snapshot!("state_03_120x34", view);

    // Same selection under the default view: the subagent takes the column over
    // and says whose conversation is on screen.
    state.config.ui.agent_view = pacode_types::config::AgentView::Replace;
    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    assert!(
        view.contains("agent codegen"),
        "the takeover banner must name the agent"
    );
    insta::assert_snapshot!("state_03_replace_120x34", view);
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
    // A running task's rendered duration is `now - started`, so start it
    // relative to now — a fixed epoch timestamp would grow every run.
    let now = pacode_types::time::now_ms();
    snapshot.tasks = vec![
        make_task(
            "tsk_1",
            "cargo test --all",
            TaskStatus::Running,
            Some(prog),
            now - 214_000,
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
    state.focus = Focus::Overlay(pacode_tui::state::Overlay::Menu {
        stack: pacode_tui::ui::menu::MenuStack::root(pacode_tui::ui::menu::MenuId::Background),
        ui: pacode_tui::ui::menu::MenuUiState::default(),
    });
    settle_focus(&mut state);

    let backend = TestBackend::new(120, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
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
            prepare_and_draw_frame(f, &mut state);
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
            prepare_and_draw_frame(f, &mut state);
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
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    let has_verb = pacode_tui::state::stats::PACMAN_VERBS
        .iter()
        .any(|v| view.contains(v));
    assert!(
        has_verb,
        "Dialog view must contain a pacman verb, but view was:\n{view}"
    );
    assert!(
        view.contains("· done "),
        "Dialog view must contain '· done ', but view was:\n{view}"
    );
    assert!(
        !view.contains("tok/s"),
        "Tokens must no longer be shown in the turn stats line"
    );
}

#[test]
fn test_snapshot_seeds_the_question_queue() {
    fn question(id: &str, text: &str) -> pacode_types::Question {
        pacode_types::Question::new(
            pacode_types::QuestionId::new(id),
            pacode_types::QuestionOrigin::new(
                AgentId::main(),
                "main",
                pacode_types::CallId::new(format!("call_{id}")),
            ),
            "",
            text,
            vec![
                pacode_types::QuestionOption::new("a", ""),
                pacode_types::QuestionOption::new("b", ""),
            ],
            false,
            0,
        )
        .expect("question")
    }

    // Attaching to a session with two waiting questions opens the picker on
    // the first — nothing is orphaned.
    let mut snapshot = make_base_snapshot();
    snapshot.pending_questions = vec![question("qst_1", "first?"), question("qst_2", "second?")];
    let mut state = create_state(120, 34, snapshot);
    assert!(matches!(
        state.focus,
        Focus::Overlay(pacode_tui::state::Overlay::QuestionPicker)
    ));
    assert_eq!(state.questions.len(), 2);
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_1")
    );

    // A later snapshot with the queue empty closes the picker again.
    let mut snapshot = make_base_snapshot();
    snapshot.pending_questions = Vec::new();
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: ClientEventContext::unscoped(),
            snapshot,
        },
        Instant::now(),
    );
    assert!(state.questions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn w128_tui_prompt_and_recovery_queues_are_bounded() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    for index in 0..128 {
        assert_eq!(
            state.input.queue_push(format!("prompt-{index}")),
            index < pacode_tui::state::input::PROMPT_QUEUE_CAP
        );
    }
    assert_eq!(
        state.input.prompt_queue.len(),
        pacode_tui::state::input::PROMPT_QUEUE_CAP
    );
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("prompt-0")
    );
    assert_eq!(
        state.input.prompt_queue.back().map(String::as_str),
        Some("prompt-15")
    );

    for index in 0..17 {
        let request = Request::UserMessage {
            text: format!("sequence-{index}"),
            images: Vec::new(),
            message_id: None,
        };
        assert_eq!(
            state.session.queue_sequence(PromptBatch::single(request)),
            index < 16
        );
    }
    assert_eq!(state.session.pending_sequences.len(), 16);

    let context = state.session_context();
    for index in 0..17 {
        state.session.begin_user_message(
            context.clone(),
            Request::UserMessage {
                text: format!("retry-{index}"),
                images: Vec::new(),
                message_id: None,
            },
        );
    }
    state.session.fail_in_flight_user_messages();
    assert_eq!(state.session.retry_requests.len(), 16);
    let first = state
        .session
        .retry_requests
        .front()
        .and_then(|batch| batch.requests.first());
    assert!(matches!(
        first,
        Some(Request::UserMessage { text, .. }) if text == "retry-1"
    ));
}

#[test]
fn w128_restart_recovery_orders_snapshot_and_discards_old_events() {
    let target = SessionId::new("ses_w128_restart");
    let mut snapshot = make_base_snapshot();
    snapshot.meta.id = target.clone();
    snapshot.seq = 0;
    snapshot.transcript.clear();
    snapshot.turn_active = false;
    snapshot.active_turn = None;
    snapshot.terminal_turn = None;
    snapshot.terminal_error = None;

    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let _pending_context = state.begin_attach(Attach::Resume {
        session: target.clone(),
    });
    state.set_pending_attach_client_epoch(1).unwrap();
    let old_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(target.clone()),
    };
    let now = Instant::now();
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: old_context.clone(),
            seq: 1,
            event: pacode_types::Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "stale".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.session.buffered_event_count(), 1);
    assert!(state.apply_client_event(
        ClientEvent::Disconnected {
            context: old_context.clone(),
            reason: "lost".into(),
        },
        now,
    ));
    assert!(state.apply_client_event(
        ClientEvent::Reconnecting {
            context: old_context.clone(),
            attempt: 1,
        },
        now,
    ));
    let new_context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(target.clone()),
    };
    assert!(state.apply_client_event(
        ClientEvent::Connected {
            context: new_context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        now,
    ));
    let mut fresh_snapshot = snapshot;
    fresh_snapshot.seq = 1;
    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: new_context.clone(),
            snapshot: fresh_snapshot,
        },
        now,
    ));
    assert!(state.pending_attach_token().is_none());
    assert!(!state.toasts.iter().any(|toast| toast.title == "stale"));
    assert_eq!(state.last_event_seq, Some(1));
    assert!(state.apply_client_event(
        ClientEvent::Event {
            context: new_context,
            seq: 2,
            event: pacode_types::Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "fresh".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.last_event_seq, Some(2));
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: old_context,
            seq: 3,
            event: pacode_types::Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "late".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.last_event_seq, Some(2));
}

#[test]
fn w128_tui_delta_burst_coalesces_into_one_cell() {
    let mut snapshot = make_base_snapshot();
    snapshot.seq = 10;
    snapshot.turn_active = true;
    snapshot.transcript = vec![TranscriptItem {
        seq: 11,
        agent: AgentId::main(),
        ts_ms: 0,
        kind: TranscriptKind::Assistant {
            text: String::new(),
            complete: false,
        },
    }];
    let mut state = create_state(120, 34, snapshot);
    let session = state.meta.as_ref().unwrap().id.clone();
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(session),
    };
    let mut expected = String::new();
    for index in 0..128 {
        let text = format!("d{index}");
        expected.push_str(&text);
        assert!(state.apply_client_event(
            ClientEvent::Event {
                context: context.clone(),
                seq: 11 + index as u64,
                event: pacode_types::Event::TextDelta {
                    agent: AgentId::main(),
                    item_seq: 11,
                    text,
                },
            },
            Instant::now(),
        ));
    }
    state.transcript.flush_stream();
    let cell = state
        .transcript
        .cells
        .iter()
        .find(|cell| cell.id == 11)
        .unwrap();
    let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind else {
        panic!("delta burst did not stay in the assistant cell")
    };
    assert_eq!(text, &expected);
    assert_eq!(state.last_event_seq, Some(138));
}

/// The model palette open on the session's route, at the smallest and the
/// usual terminal size: centred on the frame, every row inside the box.
fn palette_view(cols: u16, rows: u16) -> String {
    use pacode_types::{AuthState, ModelInfo, Pricing, ProviderAuthInfo};
    let mut state = create_state(cols, rows, make_base_snapshot());
    let priced = |provider: &str, id: &str, name: &str, input: f64, output: f64| ModelInfo {
        route: ModelRoute::new(provider, id),
        display_name: name.to_string(),
        context_window: Some(200_000),
        supports_reasoning: true,
        supports_vision: true,
        supported_efforts: Some(vec![
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
        ]),
        pricing: Some(Pricing {
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: None,
            cache_write_per_m: None,
        }),
    };
    state.models = vec![
        priced(
            "anthropic",
            "claude-3-7-sonnet",
            "Claude Sonnet 3.7",
            3.0,
            15.0,
        ),
        priced("anthropic", "claude-opus-5", "Claude Opus 5", 15.0, 75.0),
        priced("bubna", "gemini-3.8-flash", "Gemini 3.8 Flash", 0.25, 1.5),
    ];
    let auth = |id: &str, name: &str, state: AuthState| ProviderAuthInfo {
        id: id.to_string(),
        display_name: name.to_string(),
        auth_kind: "oauth".to_string(),
        detail: String::new(),
        recommended: false,
        state,
        accounts: Vec::new(),
        active: None,
    };
    state.auth_providers = vec![
        auth("anthropic", "Anthropic", AuthState::Configured),
        auth("openrouter", "OpenRouter", AuthState::NotConfigured),
    ];
    state.model_flags.clear();
    state.recent_models = vec!["anthropic/claude-opus-5".to_string()];
    pacode_tui::commands::execute(&mut state, "/model");
    settle_focus(&mut state);

    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // The box is centred on the whole frame (B25).
    let top = (0..rows)
        .find(|y| (0..cols).any(|x| buffer.cell((x, *y)).is_some_and(|c| c.symbol() == "╭")))
        .expect("the palette's top border");
    let left = (0..cols)
        .find(|x| buffer.cell((*x, top)).is_some_and(|c| c.symbol() == "╭"))
        .unwrap_or(0);
    let right = (left..cols)
        .rev()
        .find(|x| buffer.cell((*x, top)).is_some_and(|c| c.symbol() == "╮"))
        .unwrap_or(0);
    let width = right - left + 1;
    assert_eq!(width, cols.saturating_sub(4).clamp(44, 84), "{cols}x{rows}");
    assert_eq!(left, (cols - width) / 2, "{cols}x{rows}: centred");
    format!("{}", terminal.backend())
}

#[test]
fn test_model_palette_60x20() {
    let view = palette_view(60, 20);
    assert!(view.contains("Select model"), "{view}");
    assert!(view.contains("RECENT"), "{view}");
    assert!(
        view.contains("[high]"),
        "the session effort is shown: {view}"
    );
    insta::assert_snapshot!("model_palette_60x20", view);
}

#[test]
fn test_model_palette_120x34() {
    let view = palette_view(120, 34);
    for needle in [
        "Select model",
        "RECENT",
        "ANTHROPIC",
        "signed in",
        "OPENROUTER",
        "enter to sign in",
        "$3/$15",
        "effort",
    ] {
        assert!(view.contains(needle), "missing {needle}: {view}");
    }
    insta::assert_snapshot!("model_palette_120x34", view);
}
