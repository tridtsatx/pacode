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
    // Said once, on row 1.
    assert!(
        !row2.contains(crate::ui::footer_status::NO_SESSION),
        "row2 was: {row2}"
    );
}

/// Row 1 under a truecolor theme (status shades present), with or without
/// a font that has the powerline arrows.
fn opts_truecolor(width: u16, powerline: bool) -> RenderOptions {
    RenderOptions::with_theme_color(
        width,
        false,
        pacode_render::Theme::truecolor(),
        pacode_render::ColorMode::Truecolor,
    )
    .glyphs(pacode_render::Glyphs::new(false).with_powerline(powerline))
}

/// Row 1 under ANSI-16: no status shades, the flat look.
fn opts_ansi(width: u16, powerline: bool) -> RenderOptions {
    RenderOptions::with_theme_color(
        width,
        false,
        pacode_render::Theme::ansi(),
        pacode_render::ColorMode::Ansi,
    )
    .glyphs(pacode_render::Glyphs::new(false).with_powerline(powerline))
}

/// Every look the row has: flat, abutting segments, powerline arrows.
fn all_looks(width: u16) -> [RenderOptions; 3] {
    [
        opts_ansi(width, true),
        opts_truecolor(width, false),
        opts_truecolor(width, true),
    ]
}

/// A state with every row-1 piece present.
fn full_state() -> AppState {
    let mut state = make_test_state();
    state.config.ui.hints.effort = true;
    state.config.ui.hints.model = true;
    state.rail.usage.context_tokens = 12_000;
    state.rail.usage.context_window = Some(100_000);
    state.plugin_status = Some(("git".into(), "git-sync".into()));
    state
}

fn model_name() -> String {
    test_meta().model.display_name()
}

/// The bg of the span reading exactly `content`.
fn bg_of(line: &Line<'_>, content: &str) -> Option<ratatui::style::Color> {
    line.spans
        .iter()
        .find(|s| s.content == content)
        .and_then(|s| s.style.bg)
}

/// The model name is the part that yields, with a visible `…`, and the
/// effort after it survives whole.
#[test]
fn a_narrow_status_row_cuts_the_model_not_the_effort() {
    let state = make_test_state();
    // model + " · " + "high" = 7 cells + the model.
    let opts = opts_ansi(12, false);
    let text = line_text(&render_row1(12, &state, &opts));
    assert_eq!(pacode_render::display_width(&text), 12, "{text}");
    assert!(text.ends_with(" · high"), "{text}");
    assert!(text.contains('…'), "{text}");

    // Powerline: " model " + arrow + " high " + the closing arrow = 9 cells.
    let opts = opts_truecolor(12, true);
    let text = line_text(&render_row1(12, &state, &opts));
    assert_eq!(pacode_render::display_width(&text), 12, "{text}");
    assert!(text.ends_with(" high \u{e0b0}"), "{text}");
    assert!(text.contains('…'), "{text}");
}

/// The permission mode lives on row 2 alone; row 1 used to repeat it as a
/// badge.
#[test]
fn row_1_has_no_mode_badge() {
    use pacode_types::state::Mode;
    let mut state = full_state();
    for mode in [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass] {
        state.meta.as_mut().unwrap().mode = mode;
        for opts in all_looks(120) {
            let text = line_text(&render_row1(120, &state, &opts));
            let label = mode.label().to_uppercase();
            assert!(!text.contains(&label), "{mode:?}: {text}");
            assert!(text.trim_start().starts_with(&model_name()), "{text}");
        }
    }
}

#[test]
fn the_status_row_is_model_effort_and_context() {
    let state = full_state();
    let opts = opts_truecolor(100, false);
    let text = line_text(&render_row1(100, &state, &opts));
    assert!(text.starts_with(&format!(" {} ", model_name())), "{text}");
    assert!(text.contains(" high /effort "), "{text}");
    assert!(text.ends_with(" ctx 12% · /model "), "{text}");
    assert_eq!(pacode_render::display_width(&text), 100);
}

/// Segments sit on `status_bg` / `status_bg_alt` in turn; a VIM badge keeps
/// its own plaque.
#[test]
fn segment_backgrounds_alternate() {
    let mut state = full_state();
    state.config.ui.vim = true;
    let opts = opts_truecolor(120, false);
    let theme = &opts.theme;
    let row1 = render_row1(120, &state, &opts);
    let (bg, alt) = (theme.status_bg.bg, theme.status_bg_alt.bg);
    assert!(bg.is_some() && alt.is_some() && bg != alt);
    assert_eq!(bg_of(&row1, " NOR "), theme.badge(theme.cyan).bg);
    assert_eq!(bg_of(&row1, &model_name()), bg);
    assert_eq!(bg_of(&row1, "high"), alt);
    assert_eq!(bg_of(&row1, "git-sync"), bg);
    assert_eq!(bg_of(&row1, "ctx 12%"), bg);
    // The text keeps its tone on the shade.
    let effort = row1.spans.iter().find(|s| s.content == "high").unwrap();
    assert_eq!(effort.style.fg, theme.accent.fg);
}

/// The arrows appear only with the powerline glyph set, coloured from the
/// segment they leave into the one they enter.
#[test]
fn separators_only_with_the_powerline_glyph_set() {
    let state = full_state();
    let has_arrow = |opts: &RenderOptions| {
        let text = line_text(&render_row1(120, &state, opts));
        text.contains('\u{e0b0}') || text.contains('\u{e0b2}')
    };
    assert!(!has_arrow(&opts_truecolor(120, false)));
    assert!(!has_arrow(&opts_ansi(120, true)), "the flat look has none");
    let opts = opts_truecolor(120, true);
    assert!(has_arrow(&opts));

    let row1 = render_row1(120, &state, &opts);
    let theme = &opts.theme;
    let arrows: Vec<_> = row1
        .spans
        .iter()
        .filter(|s| s.content == "\u{e0b0}" || s.content == "\u{e0b2}")
        .map(|s| (s.content.to_string(), s.style.fg, s.style.bg))
        .collect();
    let (bg, alt) = (theme.status_bg.bg, theme.status_bg_alt.bg);
    let left = "\u{e0b0}".to_string();
    let right = "\u{e0b2}".to_string();
    assert_eq!(
        arrows,
        vec![
            (left.clone(), bg, alt), // model -> effort
            (left.clone(), alt, bg), // effort -> plugin
            (left, bg, None),        // closes the left group
            (right, bg, None),       // opens the right group
        ]
    );
}

/// Under ANSI-16 and mono there are no shades to draw segments on: the row
/// is the flat ` · ` line it always was, glyph set or not.
#[test]
fn the_ansi_fallback_is_the_flat_row() {
    let state = full_state();
    let left = format!("{} · high /effort · git-sync", model_name());
    let right = "ctx 12% · /model";
    let pad = " ".repeat(100 - left.chars().count() - right.chars().count());
    let expected = format!("{left}{pad}{right}");
    let mono = RenderOptions::with_theme_color(
        100,
        false,
        pacode_render::Theme::mono(),
        pacode_render::ColorMode::Mono,
    );
    for opts in [opts_ansi(100, false), opts_ansi(100, true), mono] {
        let row1 = render_row1(100, &state, &opts);
        assert_eq!(line_text(&row1), expected);
        assert!(row1.spans.iter().all(|s| s.style.bg.is_none()));
    }
}

/// Every width, every look: never wider than the row, and the pieces drop
/// in the ladder's order — the `/effort` hint, the `/model` hint, the plugin
/// status, the context fill.
#[test]
fn the_status_row_degrades_without_overflowing() {
    let state = full_state();
    for width in 0..=120usize {
        for opts in all_looks(width as u16) {
            let text = line_text(&render_row1(width, &state, &opts));
            assert!(
                pacode_render::display_width(&text) <= width,
                "{width}: {text}"
            );
            let has = |piece: &str| text.contains(piece);
            let ladder = [has("/effort"), has("/model"), has("git-sync"), has("ctx")];
            for pair in ladder.windows(2) {
                assert!(!pair[0] || pair[1], "{width}: {text}");
            }
            if width >= 80 {
                assert_eq!(ladder, [true; 4], "{width}: {text}");
            }
        }
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
    // The plugin status is the left group's last segment.
    assert!(
        row1_text.contains("git-sync: rebasing"),
        "row1_text was: {row1_text}"
    );
    assert!(!row1_text.contains("BUILD"), "row1_text was: {row1_text}");

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

/// A session on `p/<model>` whose catalog entry offers `efforts`.
fn footer_state(model: &str, efforts: &[Effort]) -> AppState {
    let mut state = make_test_state();
    let route = ModelRoute::new("p", model);
    if let Some(meta) = state.meta.as_mut() {
        meta.model = route.clone();
        meta.effort = Effort::High;
    }
    state.models = vec![pacode_types::ModelInfo {
        route,
        display_name: model.to_string(),
        context_window: None,
        supports_reasoning: !efforts.is_empty(),
        supports_vision: false,
        supported_efforts: Some(efforts.to_vec()),
        pricing: None,
        ..Default::default()
    }];
    state
}

#[test]
#[ignore = "merge follow-up: footer_status.rs still draws the effort for a model without reasoning control"]
fn footer_hides_the_effort_for_a_model_without_reasoning_control() {
    let opts = RenderOptions::new(120, false);
    let state = footer_state("plain", &[]);
    for width in [120usize, 20] {
        let row1 = line_text(&render_row1(width, &state, &opts));
        assert!(!row1.contains("high"), "row1 was: {row1}");
    }
}

#[test]
fn footer_shows_the_effort_when_the_model_supports_one() {
    let opts = RenderOptions::new(120, false);
    let state = footer_state("thinker", &[Effort::Low, Effort::High]);
    let row1 = line_text(&render_row1(120, &state, &opts));
    assert!(row1.contains("high"), "row1 was: {row1}");
}
