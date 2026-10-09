use pacode_render::RenderOptions;
use pacode_types::Config;
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus};
use pacode_types::{AgentId, AgentInfo};

use super::*;
use crate::state::AppState;

fn test_meta() -> pacode_types::SessionMeta {
    pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_footer"),
        name: None,
        cwd: "/repo".into(),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: ModelRoute::new("anthropic", "claude-sonnet-4-5"),
        effort: Effort::High,
        mode: pacode_types::state::Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

/// An attached session: the status row only speaks for a session it knows.
fn make_test_state() -> AppState {
    let config = Config::default();
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
    state.meta = Some(test_meta());
    state
}

fn render_row1(width: usize, state: &AppState, opts: &RenderOptions) -> Line<'static> {
    crate::ui::footer_status::render(width, state, opts)
}

fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// B10: with no session attached the row used to read a hardcoded
/// "Gemini 3.8 Flash" plus the default mode and effort.
#[test]
fn no_session_shows_no_model_mode_or_effort() {
    let mut state = make_test_state();
    state.meta = None;
    let opts = RenderOptions::new(120, false);
    let row1 = line_text(&render_row1(120, &state, &opts));
    assert_eq!(row1, crate::ui::footer_status::NO_SESSION);
    let row2 = line_text(&render_row2(120, &state, &opts));
    assert!(!row2.contains("ask before edits"), "row2 was: {row2}");
    assert!(!row2.contains("to cycle"), "row2 was: {row2}");
}

#[test]
fn the_status_row_is_a_mode_badge_model_effort_and_context() {
    let mut state = make_test_state();
    state.rail.usage.context_tokens = 12_000;
    state.rail.usage.context_window = Some(100_000);
    let opts = RenderOptions::new(100, false);
    let row1 = render_row1(100, &state, &opts);
    let text = line_text(&row1);
    assert!(text.starts_with(" BUILD  "), "row1 was: {text}");
    assert_eq!(row1.spans[0].style, opts.theme.badge(opts.theme.cyan));
    assert!(text.contains("high"));
    assert!(text.contains("ctx 12%"), "row1 was: {text}");
    assert_eq!(pacode_render::display_width(&text), 100);
}

#[test]
fn the_status_row_degrades_without_overflowing() {
    let state = make_test_state();
    for width in [8usize, 20, 30, 44, 60] {
        let opts = RenderOptions::new(width as u16, false);
        let text = line_text(&render_row1(width, &state, &opts));
        assert!(
            pacode_render::display_width(&text) <= width,
            "{width}: {text}"
        );
    }
}

/// B27: the hints used to spell the default keys whatever `/keys` said.
#[test]
fn hints_follow_the_keymap() {
    use crate::binding::{Action, parse_binding};
    let mut state = make_test_state();
    let opts = RenderOptions::new(160, false);
    state
        .keymap
        .set_binding(Action::CycleMode, parse_binding("alt+m").unwrap());
    state
        .keymap
        .set_binding(Action::SessionPicker, parse_binding("ctrl+o").unwrap());
    let row2 = line_text(&render_row2(160, &state, &opts));
    assert!(row2.contains("(alt+m to cycle)"), "row2 was: {row2}");
    assert!(!row2.contains("shift+tab"), "row2 was: {row2}");
    assert!(row2.contains("ctrl+o"), "row2 was: {row2}");
    assert!(!row2.contains("ctrl+p"), "row2 was: {row2}");

    // An unbound action drops out instead of naming a dead key.
    state.keymap.set_bindings(Action::CycleMode, Vec::new());
    let row2 = line_text(&render_row2(160, &state, &opts));
    assert!(!row2.contains("to cycle"), "row2 was: {row2}");

    state
        .keymap
        .set_binding(Action::FollowAgent, parse_binding("alt+y").unwrap());
    state.keymap.set_bindings(Action::Submit, Vec::new());
    assert_eq!(
        key_hints(
            &state,
            &[
                (Action::Submit, "open"),
                (Action::FollowAgent, "follow"),
                (Action::Cancel, "clear"),
            ]
        ),
        "alt+y follow · esc clear"
    );
}

#[test]
fn test_footer_render_plugin_status() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);

    // 1. Without plugin_status
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row1_text.contains("git-sync"));

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row2_text.contains("git-sync"));

    // 2. With plugin_status
    state.plugin_status = Some(("git".into(), "git-sync: rebasing".into()));

    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    // The mode badge is followed by the plugin status
    assert!(
        row1_text.contains(" BUILD  git-sync: rebasing"),
        "row1_text was: {row1_text}"
    );

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    // Chevrons + permission line followed by " · git-sync: rebasing"
    assert!(
        row2_text.contains(" · git-sync: rebasing"),
        "row2_text was: {row2_text}"
    );
    assert!(row2_text.contains("ask before edits and commands"));
}

#[test]
fn test_footer_select_agent_shows_main_and_subagents() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);
    let main_agent = AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: None,
        summary: None,
        error: None,
    };
    let sub1 = AgentInfo {
        id: AgentId::new("sub_1"),
        name: "waiter-1".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: None,
        started_at_ms: 1000,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    let sub2 = AgentInfo {
        id: AgentId::new("sub_2"),
        name: "waiter-2".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: None,
        started_at_ms: 2000,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(main_agent);
    state.rail.upsert_agent(sub1);
    state.rail.upsert_agent(sub2);

    // 1. Select main (index 0)
    state.focus = Focus::SelectAgent { index: 0 };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("main 1/3"), "text was: {text}");
    assert!(
        text.contains("enter open · alt+b follow · esc clear"),
        "text was: {text}"
    );

    // 2. Select waiter-1 (index 1)
    state.focus = Focus::SelectAgent { index: 1 };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("waiter-1 2/3"), "text was: {text}");
    assert!(
        text.contains("enter open · alt+b follow · esc clear"),
        "text was: {text}"
    );

    // 3. Panel with follow = true: hint shows alt+b release
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("sub_1")),
        follow: true,
        follow_paused: false,
    };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("pgup pause · alt+b release"),
        "text was: {text}"
    );

    // 4. Panel with follow = false: hint shows alt+b follow
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("sub_1")),
        follow: false,
        follow_paused: false,
    };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("esc back · alt+b follow · s stop"),
        "text was: {text}"
    );
}

#[test]
fn test_footer_vim_mode_badges() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);

    // With vim = false, no badge is present
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row1_text.contains("NOR"));
    assert!(!row1_text.contains("INS"));
    assert!(!row1_text.contains("VIS"));

    // Enable vim mode
    state.config.ui.vim = true;

    // Normal mode badge
    state.vim.mode = crate::state::vim::VimMode::Normal;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("NOR"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("INS"));

    // Insert mode badge
    state.vim.mode = crate::state::vim::VimMode::Insert;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("INS"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("NOR"));

    // Visual mode badge
    state.vim.mode = crate::state::vim::VimMode::Visual;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("VIS"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("NOR"));
}

#[test]
fn task_panel_footer_names_task_actions() {
    let mut state = make_test_state();
    state.focus = Focus::Panel {
        target: PanelTarget::Task(pacode_types::TaskId::new("task-1")),
        follow: false,
        follow_paused: false,
    };
    let opts = RenderOptions::new(120, false);
    let text: String = render_row2(120, &state, &opts)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("esc back · pgup scroll · k kill"), "{text}");
}

#[test]
fn completed_main_agent_is_not_counted_as_a_subagent() {
    let mut state = make_test_state();
    state.rail.show_session_stats = true;
    state.rail.usage.turns = 1;
    state.rail.usage.last_activity_ms = 1;
    state.rail.agents.push(AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Finished,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: Some(1),
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: None,
        summary: None,
        error: None,
    });
    state.rail.agents.push(AgentInfo {
        id: AgentId::new("sub-1"),
        name: "sub".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Finished,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: Some(1),
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    });
    let opts = RenderOptions::new(120, false);
    let text: String = render_row2(120, &state, &opts)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("1 agents done"), "{text}");
    assert!(!text.contains("2 agents done"), "{text}");
}

#[test]
fn slots_spans_color_by_status() {
    use pacode_types::SessionStatusKind as Kind;

    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);
    let slot = |id: &str, status| crate::state::SessionSlot {
        id: pacode_types::SessionId::new(id),
        title: id.to_string(),
        turns: 0,
        context_tokens: 0,
        agents_count: 0,
        tasks_count: 0,
        status,
    };
    state.slots[1] = Some(slot("ses_w", Kind::Working));
    state.slots[2] = Some(slot("ses_n", Kind::NeedsInput));
    state.slots[3] = Some(slot("ses_d", Kind::Done));
    state.slots[4] = Some(slot("ses_i", Kind::Idle));

    // Lit pulse phase (anim_frame 0): NeedsInput reads as accent.
    let spans = render_slots_spans(&state, &opts);
    assert_eq!(spans[0].content, "[1]");
    assert_eq!(spans[0].style, opts.theme.accent, "active slot");
    assert_eq!(spans[1].style, opts.theme.green, "working");
    assert_eq!(spans[2].style, opts.theme.accent, "needs_input lit");
    assert_eq!(spans[3].style, opts.theme.cyan, "done");
    assert_eq!(spans[4].style, opts.theme.faint, "idle");
    assert_eq!(spans[8].content, "·");
    assert_eq!(spans[8].style, opts.theme.faint, "empty");

    // Unlit pulse phase: NeedsInput dims, the rest hold their color.
    state.anim_frame = crate::state::PULSE_FRAMES / 2;
    let spans = render_slots_spans(&state, &opts);
    assert_ne!(spans[2].style, opts.theme.accent, "needs_input dimmed");
    assert_eq!(spans[1].style, opts.theme.green, "working stays put");
    assert_eq!(spans[3].style, opts.theme.cyan, "done stays put");
}

#[test]
fn active_slot_blinks_when_input_is_pending() {
    use pacode_types::SessionStatusKind as Kind;

    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);
    state.turn_active = true;
    let question = pacode_types::Question::new(
        pacode_types::QuestionId::generate(),
        pacode_types::QuestionOrigin::new(
            pacode_types::AgentId::main(),
            "main",
            pacode_types::CallId::new("call_1"),
        ),
        "pick",
        "which?",
        vec![
            pacode_types::QuestionOption::new("a", ""),
            pacode_types::QuestionOption::new("b", ""),
        ],
        false,
        1,
    )
    .unwrap();
    state.apply_event(
        1,
        pacode_types::Event::QuestionAsked(question),
        std::time::Instant::now(),
    );
    assert_eq!(state.active_slot_status(), Kind::NeedsInput);

    // Lit phase: the active badge takes the blinking accent.
    let spans = render_slots_spans(&state, &opts);
    assert_eq!(spans[0].content, "[1]");
    assert_eq!(spans[0].style, opts.theme.accent);

    // Unlit phase: it dims instead of holding accent.
    state.anim_frame = crate::state::PULSE_FRAMES / 2;
    let spans = render_slots_spans(&state, &opts);
    assert_ne!(spans[0].style, opts.theme.accent);
}
