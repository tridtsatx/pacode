use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus};
use pacode_types::{AgentId, AgentInfo, Config};

use super::*;
use crate::state::AppState;

fn make_test_state() -> AppState {
    let config = Config::default();
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
    state.awaiting_snapshot = false;
    let main_agent = AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 500,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: None,
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(main_agent);
    let subagent = AgentInfo {
        id: AgentId::new("agt_1"),
        name: "worker".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: Some("working".into()),
        started_at_ms: 1000,
        finished_at_ms: None,
        tokens_in: 100,
        tokens_out: 50,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(subagent);
    state
}

#[test]
fn test_focus_machine_transitions() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Initially Normal
    assert_eq!(state.focus, Focus::Normal);

    // 2. Alt+Down -> SelectAgent (index 0 is main)
    let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_down, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });

    // 2b. Alt+Down again -> SelectAgent (index 1 is worker subagent)
    let actions = handle_key(&mut state, alt_down, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::SelectAgent { index: 1 });

    // 3. Enter on empty prompt while on subagent -> Panel
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions, vec![Action::LoadPanel]);
    assert!(matches!(state.focus, Focus::Panel { follow: false, .. }));

    // 4. Alt+B -> Follow
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_b, now);
    assert!(actions.is_empty());
    assert!(matches!(state.focus, Focus::Panel { follow: true, .. }));

    // 5. Esc peels layers:
    // 5a. Follow -> Unfollow
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert!(matches!(state.focus, Focus::Panel { follow: false, .. }));

    // 5b. Panel -> Normal
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_dot_only_on_empty_prompt() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Empty prompt: '.' opens the Background menu level.
    let dot = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE);
    handle_key(&mut state, dot, now);
    assert!(matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));

    // Esc back to Normal
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // Non-empty prompt: '.' is inserted into text
    state.input.insert_str("cat");
    handle_key(&mut state, dot, now);
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.input.text, "cat.");
}

#[test]
fn test_slash_tab_completion() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Tab on empty prompt does nothing
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    handle_key(&mut state, tab, now);
    assert_eq!(state.input.text, "");

    // 2. Tab on slash prefix completes to matching command + trailing space
    state.input.insert_str("/mo");
    handle_key(&mut state, tab, now);
    assert_eq!(state.input.text, "/model ");
    assert_eq!(state.input.cursor, "/model ".chars().count());

    // 3. Arrow down cycles selection
    state.input.text.clear();
    state.input.cursor = 0;
    state.input.insert_str("/");
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    // Next command after model is effort
    handle_key(&mut state, tab, now);
    assert_eq!(state.input.text, "/effort ");
}

#[test]
fn test_ctrl_c_hint_lifecycle() {
    let mut state = make_test_state();
    let now = Instant::now();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    // Nothing live: no main turn, subagent finished (its report already arrived).
    for a in &mut state.rail.agents {
        a.status = match a.kind {
            AgentKind::Main => AgentStatus::Idle,
            AgentKind::Sub => AgentStatus::Finished,
        };
    }

    // 1. If text is non-empty, ctrl+c clears text and does not set hint
    state.input.insert_str("some text");
    handle_key(&mut state, ctrl_c, now);
    assert!(state.input.is_empty());
    assert_eq!(state.ctrl_c_at, None);

    // 2. Empty prompt: first ctrl+c sets ctrl_c_at
    handle_key(&mut state, ctrl_c, now);
    assert_eq!(state.ctrl_c_at, Some(now));
    assert!(!state.quit);

    // 3. Second ctrl+c within 2s exits
    let later = now + Duration::from_millis(500);
    let actions = handle_key(&mut state, ctrl_c, later);
    assert!(state.quit);
    assert_eq!(actions, vec![Action::Quit]);
}

#[test]
fn test_ctrl_c_escalation_ladder() {
    let mut state = make_test_state(); // main agent + live subagent (RunningTool)
    let now = Instant::now();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    // Main turn active: first press sends Interrupt and stops the spinner
    // optimistically instead of waiting for the daemon's TurnEnded.
    state.turn_active = true;
    let actions = handle_key(&mut state, ctrl_c, now);
    assert_eq!(actions, vec![Action::Send(Request::Interrupt)]);
    assert!(!state.turn_active);
    assert_eq!(state.ctrl_c_at, Some(now));
    assert!(!state.quit);

    // Second press inside the window while the subagent is still live:
    // escalate to InterruptAll (idempotent — more presses keep sending it).
    let later = now + Duration::from_millis(500);
    let actions = handle_key(&mut state, ctrl_c, later);
    assert_eq!(actions, vec![Action::Send(Request::InterruptAll)]);
    assert!(!state.quit);

    let later2 = later + Duration::from_millis(500);
    let actions = handle_key(&mut state, ctrl_c, later2);
    assert_eq!(actions, vec![Action::Send(Request::InterruptAll)]);
    assert!(!state.quit);

    // Once nothing is live any more, the next press inside the window exits.
    for a in &mut state.rail.agents {
        if a.kind == AgentKind::Sub {
            a.status = AgentStatus::Stopped;
        }
    }
    let later3 = later2 + Duration::from_millis(500);
    let actions = handle_key(&mut state, ctrl_c, later3);
    assert_eq!(actions, vec![Action::Quit]);
    assert!(state.quit);
}

#[test]
fn test_ctrl_c_interrupt_all_when_only_subs_live() {
    let mut state = make_test_state();
    let now = Instant::now();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    // Main turn is not running but a subagent is: the first press already
    // escalates to InterruptAll — there is nothing main-only to interrupt.
    assert!(!state.turn_active);
    let actions = handle_key(&mut state, ctrl_c, now);
    assert_eq!(actions, vec![Action::Send(Request::InterruptAll)]);
    assert_eq!(state.ctrl_c_at, Some(now));
    assert!(!state.quit);
}

#[test]
fn test_ctrl_c_window_expires() {
    let mut state = make_test_state();
    let now = Instant::now();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    for a in &mut state.rail.agents {
        a.status = AgentStatus::Finished;
    }

    // First press arms; a press after the 2 s window re-arms instead of quitting.
    handle_key(&mut state, ctrl_c, now);
    let stale = now + Duration::from_secs(3);
    let actions = handle_key(&mut state, ctrl_c, stale);
    assert!(actions.is_empty());
    assert!(!state.quit);
    assert_eq!(state.ctrl_c_at, Some(stale));
}

#[test]
fn test_shift_up_down_scrolls_input() {
    let mut state = make_test_state(); // 120x34: input shows up to 6 lines
    let now = Instant::now();
    state
        .input
        .insert_str("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10");

    let shift_down = KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT);
    for _ in 0..10 {
        let actions = handle_key(&mut state, shift_down, now);
        assert!(actions.is_empty());
    }
    // 10 wrapped lines, 6 visible rows -> max scroll is 4.
    assert_eq!(state.input.input_scroll, 4);

    let shift_up = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
    handle_key(&mut state, shift_up, now);
    assert_eq!(state.input.input_scroll, 3);

    // Saturates at 0.
    for _ in 0..10 {
        handle_key(&mut state, shift_up, now);
    }
    assert_eq!(state.input.input_scroll, 0);

    // Plain Down is history/completion navigation, not input scrolling.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(state.input.input_scroll, 0);
}

#[test]
fn test_effort_picker_focus_capture() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Submit `/effort` without arguments to open picker
    state.input.insert_str("/effort");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert!(actions.is_empty());
    // Initial index based on make_test_state() default effort (Effort::Medium -> index 1)
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::EffortPicker { .. })
    ));

    // Set to index 1 (medium) for controlled testing
    state.focus = Focus::Overlay(Overlay::EffortPicker { index: 1 });

    // 2. Left arrow moves index to 0 (low) and does NOT leak to input
    let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    let actions = handle_key(&mut state, left, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::EffortPicker { index: 0 })
    );
    assert!(state.input.is_empty());

    // 3. Typing letters does NOT leak into input
    let char_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_x, now);
    assert!(actions.is_empty());
    assert!(state.input.is_empty());

    // 4. 'l' key moves right (vim navigation)
    let char_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
    handle_key(&mut state, char_l, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::EffortPicker { index: 1 })
    );

    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    handle_key(&mut state, right, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::EffortPicker { index: 2 })
    ); // high

    // 5. Enter sends SetEffort(High) and returns focus to Normal
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::SetEffort(Effort::High))]
    );
    assert_eq!(state.focus, Focus::Normal);

    // 6. Esc cancels picker without sending
    state.focus = Focus::Overlay(Overlay::EffortPicker { index: 3 });
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let actions = handle_key(&mut state, esc, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn the_effort_cursor_skips_levels_the_model_does_not_accept() {
    // A model that takes only low and high: pressing "smarter" from low must
    // land on high, not on medium, and pressing it again must wrap to low —
    // the first *available* level, not the bottom of the ladder.
    let mut state = make_test_state();
    let now = Instant::now();
    state.models = vec![pacode_types::ModelInfo {
        route: ModelRoute::new("p", "sparse"),
        display_name: "Sparse".into(),
        context_window: Some(1000),
        supports_reasoning: true,
        supports_vision: false,
        supported_efforts: Some(vec![Effort::Low, Effort::High]),
        pricing: None,
    }];
    // The session has to be *on* the sparse model, or the catalog lookup that
    // answers "what does this model accept" has nothing to match against and
    // every rung looks available.
    state.meta = Some(pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_effort"),
        name: None,
        cwd: "/tmp".into(),
        git_branch: None,
        model: ModelRoute::new("p", "sparse"),
        effort: Effort::Medium,
        mode: pacode_types::state::Mode::Build,
        created_at_ms: 1,
        updated_at_ms: 1,
        first_prompt: None,
        zdr: false,
    });

    state.input.insert_str("/effort");
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    let Focus::Overlay(Overlay::EffortPicker { index }) = &state.focus else {
        panic!("expected the effort ladder, got {:?}", state.focus);
    };
    assert_eq!(
        Effort::ALL[*index],
        Effort::Medium,
        "the ladder opens on the current level, supported or not"
    );

    let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    handle_key(&mut state, right, now);
    let Focus::Overlay(Overlay::EffortPicker { index }) = &state.focus else {
        panic!("the ladder closed on a move");
    };
    assert_eq!(
        Effort::ALL[*index],
        Effort::High,
        "medium is not supported, so the cursor must skip it"
    );

    handle_key(&mut state, right, now);
    let Focus::Overlay(Overlay::EffortPicker { index }) = &state.focus else {
        panic!("the ladder closed on a move");
    };
    assert_eq!(
        Effort::ALL[*index],
        Effort::Low,
        "past the last available level the cursor wraps to the first available one"
    );
}

#[test]
fn test_overlay_model_picker_navigation_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.models = vec![
        pacode_types::ModelInfo {
            route: ModelRoute::new("p", "m1"),
            display_name: "Model 1".into(),
            context_window: Some(1000),
            supports_reasoning: true,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        },
        pacode_types::ModelInfo {
            route: ModelRoute::new("p", "m2"),
            display_name: "Model 2".into(),
            context_window: Some(1000),
            supports_reasoning: true,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        },
    ];

    // Open via slash command `/model` + Enter
    state.input.insert_str("/model");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: String::new(),
            index: 0,
            for_zdr: false,
        })
    );

    // Down key changes index to 1 and keeps focus on overlay
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: String::new(),
            index: 1,
            for_zdr: false,
        })
    );

    // Esc closes it
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_mode_picker_navigation_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Open via slash command `/mode` + Enter
    state.input.insert_str("/mode");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModePicker { index: 0 })
    );

    // Down key changes index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModePicker { index: 1 })
    );

    // Esc closes it
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_settings_page_opens_navigates_and_escapes() {
    let mut state = make_test_state();
    let now = Instant::now();

    // `/config` and `/settings` both open the settings page — the full one with
    // sections, search and a description line, not the hub's level. Two
    // settings screens is one too many.
    state.input.insert_str("/config");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    let Focus::Overlay(Overlay::Settings { .. }) = &state.focus else {
        panic!("expected the settings page, got {:?}", state.focus);
    };

    // Down moves the cursor to the second row of the first section.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    let Focus::Overlay(Overlay::Settings { ui }) = &state.focus else {
        panic!("expected the settings page, got {:?}", state.focus);
    };
    assert_eq!(ui.cursor(), 1, "the cursor did not move off the first row");

    // Esc closes it
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_session_picker_navigation_focus_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();
    let meta1 = pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_1"),
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        model: ModelRoute::new("p", "m"),
        effort: Effort::Medium,
        mode: pacode_types::Mode::Build,
        created_at_ms: 1000,
        updated_at_ms: 1000,
        name: Some("Session 1".into()),
        first_prompt: None,
        zdr: false,
    };
    let meta2 = pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_2"),
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        model: ModelRoute::new("p", "m"),
        effort: Effort::Medium,
        mode: pacode_types::Mode::Build,
        created_at_ms: 2000,
        updated_at_ms: 2000,
        name: Some("Session 2".into()),
        first_prompt: None,
        zdr: false,
    };
    state.sessions = vec![meta1, meta2];

    // Open via slash command `/sessions` + Enter — the menu level now (T8).
    state.input.insert_str("/sessions");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(
        stack.current().map(|l| l.id),
        Some(crate::ui::menu::MenuId::Sessions)
    );

    // Keys do NOT leak to input!
    let char_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    handle_key(&mut state, char_a, now);
    assert!(state.input.is_empty());
    // 'a' went to the menu's local filter query
    let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(ui.query, "a");

    // Backspace clears query
    let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
    handle_key(&mut state, backspace, now);

    // Down key moves the cursor to the second session row
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(ui.cursor, crate::ui::menu::RowId::new("ses_1"));

    // Esc closes it
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // Open via ctrl+p
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_p, now);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(
        stack.current().map(|l| l.id),
        Some(crate::ui::menu::MenuId::Sessions)
    );
    handle_key(&mut state, down, now);
    let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(ui.cursor, crate::ui::menu::RowId::new("ses_1"));
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_files_alt_f_navigation_enter_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();

    state
        .files
        .observe_tool_item("read", &serde_json::json!({"path": "src/first.rs"}), 100);
    state
        .files
        .observe_tool_item("write", &serde_json::json!({"path": "src/second.rs"}), 200);

    // 1. alt+f opens the Files menu level.
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    handle_key(&mut state, alt_f, now);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("alt+f did not open the menu: {:?}", state.focus);
    };
    assert_eq!(stack.current().unwrap().id, crate::ui::menu::MenuId::Files);

    // 2. Down moves the cursor past the "src" section to the second file.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus else {
        panic!("menu closed on Down");
    };
    assert_eq!(ui.cursor, crate::ui::menu::RowId::new("src/first.rs"));

    // 3. Typing letters does NOT leak to input
    let char_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    handle_key(&mut state, char_z, now);
    assert!(state.input.is_empty());

    // 4. Enter opens the selected file's worktree diff in the panel (§5.5).
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert!(matches!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::FileDiff { ref path },
            ..
        } if path == "src/first.rs"
    ));
    assert_eq!(state.files.diff_want.as_deref(), Some("src/first.rs"));

    // 5. Esc on the panel returns to Normal and drops the diff target.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.panel.target, None);

    // 6. Verify alt+b is follow (does not open files)
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    handle_key(&mut state, alt_b, now);
    assert!(!matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));
}

#[test]
fn test_alt_b_toggles_follow() {
    let mut state = make_test_state();
    let now = Instant::now();
    let subagent_id = AgentId::new("agt_1");

    // Panel open on subagent with follow = false
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(subagent_id.clone()),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(PanelTarget::Agent(subagent_id.clone()));

    // 1. alt+b toggles follow to true
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_b, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id.clone()),
            follow: true,
            follow_paused: false,
        }
    );

    // 2. alt+b toggles follow back to false
    let actions = handle_key(&mut state, alt_b, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id.clone()),
            follow: false,
            follow_paused: false,
        }
    );

    // 3. alt+b on SelectAgent for a subagent opens panel with follow = true
    state.focus = Focus::SelectAgent { index: 1 };
    let actions = handle_key(&mut state, alt_b, now);
    assert_eq!(actions, vec![Action::LoadPanel]);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id),
            follow: true,
            follow_paused: false,
        }
    );

    // 4. alt+b on SelectAgent for main returns to Focus::Normal without opening panel
    state.focus = Focus::SelectAgent { index: 0 };
    let actions = handle_key(&mut state, alt_b, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.panel.target, None);
}

#[test]
fn test_alt_f_opens_files_overlay() {
    let mut state = make_test_state();
    let now = Instant::now();

    assert_eq!(state.focus, Focus::Normal);
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_f, now);
    assert!(actions.is_empty());
    // The entry opens the Files menu level.
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::Menu { ref stack, .. })
            if stack.current().is_some_and(|l| l.id == crate::ui::menu::MenuId::Files)
    ));
}

#[test]
fn test_alt_down_cycle_selects_main_first_then_subagents_and_wraps() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Add a second subagent so we have main (0), agt_1 (1), agt_2 (2)
    let subagent2 = AgentInfo {
        id: AgentId::new("agt_2"),
        name: "tester".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: Some("testing".into()),
        started_at_ms: 2000,
        finished_at_ms: None,
        tokens_in: 50,
        tokens_out: 25,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(subagent2);

    assert_eq!(state.focus, Focus::Normal);
    let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
    let alt_up = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);

    // 1. alt+Down from Focus::Normal selects main first (index 0)
    handle_key(&mut state, alt_down, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });

    // 2. Next alt+Down selects first subagent (index 1: agt_1)
    handle_key(&mut state, alt_down, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 1 });

    // 3. Next alt+Down selects second subagent (index 2: agt_2)
    handle_key(&mut state, alt_down, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 2 });

    // 4. Next alt+Down wraps back to main (index 0)
    handle_key(&mut state, alt_down, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });

    // 5. alt+Up from index 0 wraps to the last subagent (index 2: agt_2)
    handle_key(&mut state, alt_up, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 2 });

    // 6. alt+Up moves back to index 1
    handle_key(&mut state, alt_up, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 1 });

    // 7. alt+Up moves back to main (index 0)
    handle_key(&mut state, alt_up, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });

    // 8. ctrl+j and ctrl+k cycle identically
    let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    let ctrl_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_j, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 1 });
    handle_key(&mut state, ctrl_k, now);
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });
}

#[test]
fn test_enter_on_main_returns_to_normal() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Select main
    state.focus = Focus::SelectAgent { index: 0 };
    state.panel.target = Some(PanelTarget::Agent(AgentId::new("agt_1")));

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);

    assert!(actions.is_empty(), "Enter on main must not emit LoadPanel");
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.panel.target, None);
}

#[test]
fn test_enter_on_subagent_opens_panel() {
    let mut state = make_test_state();
    let now = Instant::now();
    let subagent_id = AgentId::new("agt_1");

    // Select subagent (index 1)
    state.focus = Focus::SelectAgent { index: 1 };
    state.panel.target = None;

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);

    assert_eq!(actions, vec![Action::LoadPanel]);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id.clone()),
            follow: false,
            follow_paused: false,
        }
    );
    assert_eq!(state.panel.target, Some(PanelTarget::Agent(subagent_id)));
}

#[test]
fn test_overlay_rail_and_help_navigation_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();

    // RailOverlay
    state.focus = Focus::Overlay(Overlay::RailOverlay {
        selected: AgentId::main(),
    });
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay {
            selected: AgentId::new("agt_1"),
        })
    );
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // Help via slash command `/help` + Enter — a menu level now (§3.5).
    state.input.insert_str("/help");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(stack.depth(), 1);
    assert_eq!(
        stack.current().map(|l| l.id),
        Some(crate::ui::menu::MenuId::Help)
    );

    handle_key(&mut state, down, now);
    // Still focused — the menu owns the key.
    assert!(matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_rail_overlay_selects_main_and_subagents() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.focus = Focus::Overlay(Overlay::RailOverlay {
        selected: AgentId::main(),
    });

    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay { ref selected }) if selected == &AgentId::new("agt_1")
    ));

    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        now,
    );
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay { ref selected }) if selected == &AgentId::main()
    ));

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);

    state.focus = Focus::Overlay(Overlay::RailOverlay {
        selected: AgentId::main(),
    });
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        now,
    );
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    assert_eq!(actions, vec![Action::LoadPanel]);
    assert!(matches!(state.focus, Focus::Panel { .. }));

    state.focus = Focus::Overlay(Overlay::RailOverlay {
        selected: AgentId::main(),
    });
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        now,
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_mcp_picker_keys() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Open via slash command `/mcp` + Enter — the shared menu's MCP level
    // answers it.
    state.input.insert_str("/mcp");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions, vec![Action::Send(Request::ListMcpServers)]);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the MCP menu level, got {:?}", state.focus);
    };
    assert_eq!(
        stack.current().map(|l| l.id),
        Some(crate::ui::menu::MenuId::Mcp)
    );

    // 2. Populate servers — the menu view reads `state.mcp_servers` live.
    let server1 = pacode_types::McpServerInfo {
        name: "srv1".into(),
        status: "ready".into(),
        error: None,
        tools: 3,
        resources: 1,
        prompts: 2,
        prompt_names: vec!["p1".into(), "p2".into()],
    };
    let server2 = pacode_types::McpServerInfo {
        name: "srv2".into(),
        status: "disabled".into(),
        error: None,
        tools: 0,
        resources: 0,
        prompts: 0,
        prompt_names: vec![],
    };
    state.mcp_servers = vec![server1, server2];

    let menu_cursor = |state: &AppState| match &state.focus {
        Focus::Overlay(Overlay::Menu { ui, .. }) => ui.cursor.clone(),
        other => panic!("expected the MCP menu level, got {other:?}"),
    };

    // 3. Down moves the cursor to srv2
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    let actions = handle_key(&mut state, down, now);
    assert!(actions.is_empty());
    assert_eq!(menu_cursor(&state), crate::ui::menu::RowId::new("srv2"));

    // Typing letters doesn't leak into input
    let char_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    handle_key(&mut state, char_z, now);
    assert!(state.input.is_empty());

    // 4. Up moves the cursor back to srv1
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert_eq!(menu_cursor(&state), crate::ui::menu::RowId::new("srv1"));

    // 5. 'r' on srv1 sends RestartMcpServer + ListMcpServers
    let char_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_r, now);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::RestartMcpServer {
                server: "srv1".into()
            }),
            Action::Send(Request::ListMcpServers),
        ]
    );

    // 6. Enter on srv1 (status "ready") toggles to disabled (enabled = false)
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::SetMcpServerEnabled {
                server: "srv1".into(),
                enabled: false,
            }),
            Action::Send(Request::ListMcpServers),
        ]
    );

    // 7. Enter on srv2 (status "disabled") toggles to enabled (enabled = true)
    handle_key(&mut state, down, now);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::SetMcpServerEnabled {
                server: "srv2".into(),
                enabled: true,
            }),
            Action::Send(Request::ListMcpServers),
        ]
    );

    // 8. Esc closes the menu level
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let actions = handle_key(&mut state, esc, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_plugins_picker_keys() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Open via slash command `/plugins` + Enter
    state.input.insert_str("/plugins");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    // With a marketplace configured, opening the screen also asks it for its list.
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::ListPlugins),
            Action::Send(Request::BrowseMarketplace {
                source: "tridtsatx/pacode-plugins".to_string(),
                query: String::new(),
            }),
        ]
    );
    // §5.2: `/plugins` opens the menu-base Plugins level.
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::Menu {
            stack: crate::ui::menu::MenuStack::root(crate::ui::menu::MenuId::Plugins),
            ui: crate::ui::menu::MenuUiState::default(),
        })
    );

    // 2. Populate plugins — the menu view reads `state.plugins` live.
    let p1 = pacode_types::PluginInfo {
        name: "plug1".into(),
        version: "1.0.0".into(),
        kind: "lua".into(),
        tools: vec![],
        commands: vec!["c1".into()],
        error: None,
        loaded: true,
        ..pacode_types::PluginInfo::default()
    };
    let p2 = pacode_types::PluginInfo {
        name: "plug2".into(),
        version: "2.0.0".into(),
        kind: "wasm".into(),
        tools: vec![],
        commands: vec![],
        error: None,
        loaded: true,
        ..pacode_types::PluginInfo::default()
    };
    state.plugins = vec![p1, p2];

    // 3. Down moves the cursor to the second Installed row
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    let Focus::Overlay(Overlay::Menu { ui, .. }) = &state.focus else {
        panic!("expected the Plugins menu level, got {:?}", state.focus);
    };
    assert_eq!(ui.cursor, crate::ui::menu::RowId::new("installed:plug2"));

    // Letters do not leak to input
    let char_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    handle_key(&mut state, char_a, now);
    assert!(state.input.is_empty());

    // 4. Esc closes the menu level
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_plugins_picker_x_uninstalls_only_installed_plugins() {
    let mut state = make_test_state();
    let now = Instant::now();

    // A plugin dropped into the dir by hand has no install record — `x` has
    // nothing to remove.
    let manual = pacode_types::PluginInfo {
        name: "manual".into(),
        version: "1.0.0".into(),
        kind: "lua".into(),
        loaded: true,
        ..pacode_types::PluginInfo::default()
    };
    // A marketplace-installed plugin does — `x` removes it and refreshes.
    let installed = pacode_types::PluginInfo {
        name: "playwright".into(),
        version: "2.1.0".into(),
        source: "owner/repo".into(),
        installed_at_ms: 1_700_000_000_000,
        ..pacode_types::PluginInfo::default()
    };
    state.plugins = vec![manual, installed];
    state.focus = Focus::Overlay(Overlay::Menu {
        stack: crate::ui::menu::MenuStack::root(crate::ui::menu::MenuId::Plugins),
        ui: crate::ui::menu::MenuUiState::default(),
    });

    let x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, x, now);
    assert!(
        actions.is_empty(),
        "no install record, nothing to uninstall"
    );

    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    let actions = handle_key(&mut state, x, now);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::UninstallPlugin {
                name: "playwright".into()
            }),
            Action::Send(Request::ListPlugins),
        ]
    );
}

#[test]
fn test_vim_disabled_behavior_is_identical() {
    let mut state = make_test_state();
    let now = Instant::now();
    assert!(!state.config.ui.vim);

    // Typing 'w', 'b', 'd', 'x' enters them as text characters
    for ch in ['w', 'b', 'd', 'x'] {
        let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        let actions = handle_key(&mut state, key, now);
        assert!(actions.is_empty());
    }
    assert_eq!(state.input.text, "wbdx");
    assert_eq!(state.input.cursor, 4);

    // Esc clears selection and returns empty actions without altering text
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let actions = handle_key(&mut state, esc, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "wbdx");

    // Enter submits the prompt exactly as before
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::UserMessage {
            text: "wbdx".into(),
            images: Vec::new(),
            message_id: None,
        })]
    );
    assert!(state.input.is_empty());
}

#[test]
fn test_overlay_intercepts_keys_before_vim_when_vim_enabled() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.config.ui.vim = true;

    // Open the Help menu level
    state.focus = Focus::Overlay(Overlay::Menu {
        stack: crate::ui::menu::MenuStack::root(crate::ui::menu::MenuId::Help),
        ui: crate::ui::menu::MenuUiState::default(),
    });

    // Pressing 'j' or 'x' in the menu does not go to vim mode and does not
    // modify input (Help's local filter owns printable keys — they land in
    // the query, not the prompt).
    let char_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_j, now);
    assert!(actions.is_empty());
    assert!(state.input.is_empty());
    assert!(matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));

    let char_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_x, now);
    assert!(actions.is_empty());
    assert!(state.input.is_empty());
    assert!(matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));

    // Esc closes overlay first, returning to Normal prompt
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let actions = handle_key(&mut state, esc, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_vim_enabled_keys_routing() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.config.ui.vim = true;
    state.input.text = "hello world".to_string();
    state.input.cursor = 0;

    // Normal mode motion 'w' moves cursor, does NOT insert 'w'
    let char_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_w, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.cursor, 6);
    assert_eq!(state.input.text, "hello world");

    // 'i' enters insert mode
    let char_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
    handle_key(&mut state, char_i, now);
    assert_eq!(state.vim.mode, crate::state::vim::VimMode::Insert);

    // In insert mode, characters are inserted into prompt
    let char_excl = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE);
    handle_key(&mut state, char_excl, now);
    assert_eq!(state.input.text, "hello !world");

    // Esc exits insert mode to normal mode
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.vim.mode, crate::state::vim::VimMode::Normal);

    // Enter in normal mode submits the prompt
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::UserMessage {
            text: "hello !world".into(),
            images: Vec::new(),
            message_id: None,
        })]
    );
    assert!(state.input.is_empty());
}

#[test]
fn test_default_binding_newline() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.input.insert_str("line1");

    // Shift+Enter inserts newline
    let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    let actions = handle_key(&mut state, shift_enter, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "line1\n");

    // Alt+Enter inserts newline
    let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_enter, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "line1\n\n");
}

#[test]
fn test_default_binding_clear_input() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.input.insert_str("some input to clear");

    let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_u, now);
    assert!(actions.is_empty());
    assert!(state.input.is_empty());
    assert_eq!(state.input.cursor, 0);
}

#[test]
fn test_default_binding_cycle_mode() {
    let mut state = make_test_state();
    let now = Instant::now();
    let initial_mode = state.mode();
    let expected_next = initial_mode.next();

    // Shift+Tab cycles mode
    let shift_tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
    let actions = handle_key(&mut state, shift_tab, now);
    assert_eq!(actions, vec![Action::Send(Request::SetMode(expected_next))]);

    // BackTab cycles mode
    let backtab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
    let actions = handle_key(&mut state, backtab, now);
    assert_eq!(actions, vec![Action::Send(Request::SetMode(expected_next))]);
}

#[test]
fn test_default_binding_stop_agent_and_kill_task() {
    let mut state = make_test_state();
    let now = Instant::now();
    let agent_id = AgentId::new("agt_1");
    let task_id = pacode_types::TaskId::new("task_42");

    // 's' when focused on agent panel stops agent
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(agent_id.clone()),
        follow: false,
        follow_paused: false,
    };
    let key_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_s, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::StopAgent(agent_id.clone()))]
    );

    // 'k' when focused on task panel kills task
    state.focus = Focus::Panel {
        target: PanelTarget::Task(task_id.clone()),
        follow: false,
        follow_paused: false,
    };
    let key_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_k, now);
    assert_eq!(actions, vec![Action::Send(Request::KillTask(task_id))]);

    // 'k' on the Background menu level kills the selected task — the row id
    // is the daemon TaskId (§5.9).
    let bg_task_id = pacode_types::TaskId::new("task_bg");
    let task = pacode_types::TaskInfo {
        id: bg_task_id.clone(),
        session: pacode_types::SessionId::new("ses_1"),
        owner: agent_id,
        label: "build".into(),
        command: "cargo build".into(),
        cwd: std::path::PathBuf::from("/tmp"),
        status: pacode_types::state::TaskStatus::Running,
        backgrounded: true,
        exit_code: None,
        started_at_ms: 0,
        ended_at_ms: None,
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: std::path::PathBuf::from("/tmp/out"),
        output_bytes: 0,
        acked: false,
    };
    state.rail.tasks.push(task);
    state.focus = Focus::Overlay(Overlay::Menu {
        stack: crate::ui::menu::MenuStack::root(crate::ui::menu::MenuId::Background),
        ui: crate::ui::menu::MenuUiState::default(),
    });
    let actions = handle_key(&mut state, key_k, now);
    assert_eq!(actions, vec![Action::Send(Request::KillTask(bg_task_id))]);
}

#[test]
fn test_default_binding_scrolling() {
    let mut state = make_test_state();
    let now = Instant::now();

    // In normal focus: PageUp, PageDown, Home, End
    let pgup = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
    let pgdn = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
    let home = KeyEvent::new(KeyCode::Home, KeyModifiers::NONE);
    let end = KeyEvent::new(KeyCode::End, KeyModifiers::NONE);

    assert_eq!(handle_key(&mut state, pgup, now), vec![]);
    assert_eq!(handle_key(&mut state, pgdn, now), vec![]);
    assert_eq!(handle_key(&mut state, home, now), vec![]);
    assert_eq!(handle_key(&mut state, end, now), vec![]);

    // In panel with follow=true: PageUp pauses follow, End unpauses
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("agt_1")),
        follow: true,
        follow_paused: false,
    };
    handle_key(&mut state, pgup, now);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(AgentId::new("agt_1")),
            follow: true,
            follow_paused: true,
        }
    );
    handle_key(&mut state, end, now);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(AgentId::new("agt_1")),
            follow: true,
            follow_paused: false,
        }
    );
}

#[test]
fn test_default_binding_copy_selection() {
    let mut state = make_test_state();
    let now = Instant::now();

    // No selection and an empty prompt -> does nothing, returns empty
    let copy_key = KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    let actions = handle_key(&mut state, copy_key, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.selection.copy_request,
        crate::state::selection::CopyRequest::None
    );
    assert!(state.pending_copy.is_none());

    // No selection but text in the prompt -> the input is what gets copied.
    state.input.text = "draft prompt".to_string();
    state.input.cursor = state.input.text.chars().count();
    let actions = handle_key(&mut state, copy_key, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.pending_copy,
        Some(crate::state::PendingCopy {
            text: "draft prompt".to_string(),
            auto: false,
        })
    );
    state.pending_copy = None;

    // Make selection active and non-empty — it wins over the prompt text.
    let rect = ratatui::layout::Rect::new(0, 0, 80, 24);
    state.selection.start(0, 0, rect, 0);
    state.selection.drag(5, 0, rect, 0);
    state.selection.finish();
    assert!(state.selection.is_active() && !state.selection.is_empty());

    let actions = handle_key(&mut state, copy_key, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.selection.copy_request,
        crate::state::selection::CopyRequest::Explicit
    );
    assert!(state.pending_copy.is_none());
}

#[test]
fn test_user_override_in_keys_config() {
    let mut config = Config::default();
    config
        .keys
        .bindings
        .insert("follow_agent".to_string(), "ctrl+g".to_string());
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
    let subagent_id = AgentId::new("agt_1");
    state.rail.upsert_agent(AgentInfo {
        id: subagent_id.clone(),
        name: "worker".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
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
    });
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(subagent_id),
        follow: false,
        follow_paused: false,
    };
    let now = Instant::now();

    // Default alt+b no longer triggers follow
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    handle_key(&mut state, alt_b, now);
    assert!(matches!(state.focus, Focus::Panel { follow: false, .. }));

    // Overridden ctrl+g triggers follow
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_g, now);
    assert!(matches!(state.focus, Focus::Panel { follow: true, .. }));
}

#[test]
fn test_ctrl_x_chord_discarded() {
    let mut state = make_test_state();
    state.input.text = "hello".to_string();
    state.input.cursor = 5;
    let now = Instant::now();

    // 1. Press ctrl+x: enters pending chord state, returns empty actions, prompt unchanged
    let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_x, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "hello");
    assert!(state.pending_chord.is_some());

    // 2. Press 'z' (not ctrl+e): chord discarded, 'z' processed normally (inserted into prompt)
    let char_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, char_z, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "helloz");
    assert_eq!(state.input.cursor, 6);
    assert!(state.pending_chord.is_none());
}

#[test]
fn test_select_agent_shortcuts() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Add a third agent so selectable_agents has 3 entries: main, agt_1, agt_2
    let third_agent = AgentInfo {
        id: AgentId::new("agt_2"),
        name: "reviewer".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 1500,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(third_agent);
    let agents = selectable_agents(&state);
    assert_eq!(agents.len(), 3);
    assert_eq!(agents[0], AgentId::main());
    assert_eq!(agents[2], AgentId::new("agt_2"));

    // alt+3 selects the third entry of selectable_agents
    let alt_3 = KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_3, now);
    assert_eq!(actions, vec![Action::LoadPanel]);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(AgentId::new("agt_2")),
            follow: false,
            follow_paused: false,
        }
    );
    assert_eq!(
        state.panel.target,
        Some(PanelTarget::Agent(AgentId::new("agt_2")))
    );

    // alt+1 selects main (back to Focus::Normal, panel target cleared)
    let alt_1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_1, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.panel.target, None);

    // alt+9 with only 2 agents does nothing at all (state unchanged, no toast pushed)
    // Remove the 3rd agent so there are only 2 agents
    state.rail.agents.retain(|a| a.id != AgentId::new("agt_2"));
    assert_eq!(selectable_agents(&state).len(), 2);

    state.dirty = false;
    let focus_before = state.focus.clone();
    let panel_before = state.panel.target.clone();
    let toasts_len_before = state.toasts.len();

    let alt_9 = KeyEvent::new(KeyCode::Char('9'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_9, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, focus_before);
    assert_eq!(state.panel.target, panel_before);
    assert_eq!(state.toasts.len(), toasts_len_before);
    assert!(!state.dirty);
}

#[test]
fn test_session_slot_switching() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.cwd = std::path::PathBuf::from("/test/cwd");

    // Initially slot 1 is active (active_slot = 0)
    assert_eq!(state.active_slot, 0);

    // Switching to the already-active slot (ctrl+alt+1) emits nothing
    let ctrl_alt_1 = KeyEvent::new(
        KeyCode::Char('1'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    let actions = handle_key(&mut state, ctrl_alt_1, now);
    assert!(actions.is_empty());
    assert_eq!(state.active_slot, 0);

    // Switching to an empty slot emits one ordered transition action
    let ctrl_alt_2 = KeyEvent::new(
        KeyCode::Char('2'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    let actions = handle_key(&mut state, ctrl_alt_2, now);
    assert_eq!(state.active_slot, 1);
    assert_eq!(
        actions,
        vec![Action::SwitchSession {
            attach: pacode_types::Attach::New {
                cwd: std::path::PathBuf::from("/test/cwd"),
                model: None,
                effort: None,
                mode: None,
            },
        }]
    );

    // Populate slot 3 with an occupied SessionSlot
    let slot_3_session = pacode_types::SessionId::new("sess_occupied_3");
    state.slots[2] = Some(crate::state::SessionSlot {
        id: slot_3_session.clone(),
        title: "Session 3".to_string(),
        turns: 5,
        context_tokens: 1200,
        agents_count: 1,
        tasks_count: 0,
        status: pacode_types::SessionStatusKind::Idle,
    });

    // Switching to an occupied slot uses the same ordered transition
    let ctrl_alt_3 = KeyEvent::new(
        KeyCode::Char('3'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    let actions = handle_key(&mut state, ctrl_alt_3, now);
    assert_eq!(state.active_slot, 2);
    assert_eq!(
        actions,
        vec![Action::SwitchSession {
            attach: pacode_types::Attach::Resume {
                session: slot_3_session,
            },
        }]
    );
}

#[test]
fn test_leaving_slot_clears_transcript_cells() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Populate transcript with some cells
    state.transcript.cells.push_back(crate::state::Cell {
        id: 1,
        kind: crate::state::CellKind::Item(pacode_types::TranscriptKind::User {
            text: "test".into(),
        }),
        version: 0,
        ts_ms: 100,
        stats: None,
        perm_explain: None,
    });
    assert!(!state.transcript.cells.is_empty());

    // Switch to slot 2
    let ctrl_alt_2 = KeyEvent::new(
        KeyCode::Char('2'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    handle_key(&mut state, ctrl_alt_2, now);

    // Transcript cells must be cleared
    assert!(state.transcript.cells.is_empty());
}

#[test]
fn test_lazy_loader_scrolling() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Populate transcript with items
    let items: Vec<pacode_types::TranscriptItem> = (10..30)
        .map(|i| pacode_types::TranscriptItem {
            seq: i,
            agent: AgentId::main(),
            ts_ms: i * 10,
            kind: pacode_types::TranscriptKind::User {
                text: format!("msg {i}"),
            },
        })
        .collect();
    state.transcript.reset(items, true);
    state.transcript.record_render(30, 10);
    assert!(state.transcript.has_more_history);
    assert!(!state.transcript.loading_history);
    assert_eq!(state.transcript.oldest_seq(), Some(10));

    // Scroll to the top using ScrollTop (Home or key)
    let home = KeyEvent::new(KeyCode::Home, KeyModifiers::NONE);
    let actions = handle_key(&mut state, home, now);

    // Scrolling to the top requests the next page with the right before_seq
    assert_eq!(actions, vec![Action::LoadHistory]);
    assert!(state.transcript.loading_history);

    // A second scroll while one is pending requests nothing
    let actions_second = handle_key(&mut state, home, now);
    assert!(actions_second.is_empty());

    // Daemon reports no older history
    state.transcript.prepend(vec![], false);
    assert!(!state.transcript.loading_history);
    assert!(!state.transcript.has_more_history);

    // Once the daemon reports no older history, further scrolls request nothing
    let actions_after = handle_key(&mut state, home, now);
    assert!(actions_after.is_empty());
}

#[test]
fn test_slash_command_enter_completion_and_execution() {
    let mut state = make_test_state();
    let now = Instant::now();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    // 1. `/cl` + Enter leaves `/clear ` in the prompt and sends nothing
    state.input.insert_str("/cl");
    let actions1 = handle_key(&mut state, enter, now);
    assert!(actions1.is_empty(), "First enter must send nothing");
    assert_eq!(
        state.input.text, "/clear ",
        "First enter must complete to '/clear '"
    );
    assert_eq!(state.input.cursor, "/clear ".chars().count());

    // 2. A second Enter runs it
    // Insert a dummy cell to verify /clear clears the transcript
    state.transcript.cells.push_back(crate::state::Cell {
        id: 1,
        kind: crate::state::CellKind::Item(pacode_types::TranscriptKind::User {
            text: "test".into(),
        }),
        version: 0,
        ts_ms: 0,
        stats: None,
        perm_explain: None,
    });
    assert!(!state.transcript.cells.is_empty());
    let actions2 = handle_key(&mut state, enter, now);
    assert!(actions2.is_empty());
    assert!(
        state.input.is_empty(),
        "Second enter must submit and clear the prompt"
    );
    assert!(
        state.transcript.cells.is_empty(),
        "/clear must have executed and cleared cells"
    );

    // 3. `/clear` + Enter (already complete, one exact match) runs it immediately rather than requiring two presses
    state.transcript.cells.push_back(crate::state::Cell {
        id: 2,
        kind: crate::state::CellKind::Item(pacode_types::TranscriptKind::User {
            text: "test2".into(),
        }),
        version: 0,
        ts_ms: 0,
        stats: None,
        perm_explain: None,
    });
    state.input.insert_str("/clear");
    let actions_exact = handle_key(&mut state, enter, now);
    assert!(actions_exact.is_empty());
    assert!(
        state.input.is_empty(),
        "Exact match must run immediately on single Enter"
    );
    assert!(
        state.transcript.cells.is_empty(),
        "/clear must have executed immediately"
    );

    // 4. `/x` matching nothing + Enter behaves as today (attempts execution, produces notice)
    state.input.insert_str("/x");
    let actions_unknown = handle_key(&mut state, enter, now);
    assert!(actions_unknown.is_empty());
    assert!(
        state.input.is_empty(),
        "Unknown command is submitted and input is cleared"
    );
    // Notice item for unknown command is added to transcript
    let last_cell = state.transcript.cells.back().expect("cell exists");
    match &last_cell.kind {
        crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice { text, .. }) => {
            assert!(
                text.contains("Unknown command: /x"),
                "expected unknown command notice, got: '{text}'"
            );
        }
        other => panic!("expected notice, got {other:?}"),
    }
}

#[test]
fn test_keymap_delete_word_actions() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. ctrl+delete deletes word forward
    state.input.insert_str("foo   bar baz");
    state.input.cursor = 0;
    let ctrl_del = KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_del, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "bar baz");
    assert_eq!(state.input.cursor, 0);

    // 2. ctrl+w deletes word backward via keymap action DeleteWordBack
    state.input.cursor = state.input.text.chars().count();
    let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_w, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "bar ");
    assert_eq!(state.input.cursor, 4);

    // 3. Vim-mode path is unaffected
    state.config.ui.vim = true;
    state.vim.mode = crate::state::vim::VimMode::Normal;
    state.input.text = "hello world".to_string();
    state.input.cursor = 0;

    // Normal mode 'w' motion moves cursor forward, vim handles it
    let char_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
    handle_key(&mut state, char_w, now);
    assert_eq!(state.input.cursor, 6);
    assert_eq!(state.input.text, "hello world");
}

#[test]
fn test_keymap_word_move_and_boundary_actions() {
    let mut state = make_test_state();
    let now = Instant::now();

    // ctrl+left/right jump words (readline backward/forward-word).
    state.input.insert_str("foo   bar baz");
    let ctrl_left = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_left, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.cursor, 10); // start of "baz"
    let ctrl_right = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_right, now);
    assert_eq!(state.input.cursor, 13); // end of input

    // ctrl+up/down jump to whole-input boundaries.
    let ctrl_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_up, now);
    assert_eq!(state.input.cursor, 0);
    let ctrl_down = KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_down, now);
    assert_eq!(state.input.cursor, 13);
}

#[test]
fn test_keymap_delete_line_and_backspace_word_actions() {
    let mut state = make_test_state();
    let now = Instant::now();

    // alt+delete removes the current logical line (last line here, so the
    // preceding '\n' goes with it).
    state.input.insert_str("l1\nl2\nl3");
    state.input.cursor = 6; // on "l3"
    let alt_del = KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_del, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "l1\nl2");
    assert_eq!(state.input.cursor, 5);

    // ctrl+backspace and alt+backspace both delete the word to the left.
    for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        state.input.text = "foo   bar baz".to_string();
        state.input.cursor = state.input.text.chars().count();
        let key = KeyEvent::new(KeyCode::Backspace, mods);
        let actions = handle_key(&mut state, key, now);
        assert!(actions.is_empty());
        assert_eq!(state.input.text, "foo   bar ");
        assert_eq!(state.input.cursor, 10);
    }

    // Plain backspace still deletes a single char.
    let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
    handle_key(&mut state, backspace, now);
    assert_eq!(state.input.text, "foo   bar");
}

#[test]
fn test_up_down_move_within_multiline_prompt() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.input.insert_str("l1\nl2\nl3");
    // Cursor at the end (visual line 2, column 2).
    assert_eq!(state.input.cursor, 8);

    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert_eq!(state.input.cursor, 5); // end of "l2"
    handle_key(&mut state, up, now);
    assert_eq!(state.input.cursor, 2); // end of "l1"
    // First visual line: falls back to the history gate — a non-empty
    // prompt that isn't browsing history stays put.
    handle_key(&mut state, up, now);
    assert_eq!(state.input.cursor, 2);
    assert_eq!(state.input.text, "l1\nl2\nl3");
    assert_eq!(state.input.history_index, None);

    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(state.input.cursor, 5);
    handle_key(&mut state, down, now);
    assert_eq!(state.input.cursor, 8); // back to the end
    handle_key(&mut state, down, now);
    assert_eq!(state.input.cursor, 8); // last line: no-op
}

#[test]
fn test_up_down_history_edges_around_multiline_prompt() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.input.history.push_back("earlier".to_string());
    state.input.history.push_back("multi\nline".to_string());

    // Empty prompt: Up browses history as before.
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert_eq!(state.input.text, "multi\nline");
    assert_eq!(state.input.history_index, Some(1));

    // While browsing, Up keeps stepping history — the recalled entry's
    // extra visual lines do not switch to cursor movement.
    handle_key(&mut state, up, now);
    assert_eq!(state.input.text, "earlier");
    assert_eq!(state.input.history_index, Some(0));

    // Down steps forward again, then lands back on the empty draft.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(state.input.text, "multi\nline");
    handle_key(&mut state, down, now);
    assert_eq!(state.input.text, "");
    assert_eq!(state.input.history_index, None);
}

#[test]
fn test_enter_queues_when_turn_running_and_sends_when_idle() {
    let mut state = make_test_state();
    let now = Instant::now();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    // 1. Enter while idle sends as before
    state.turn_active = false;
    state.input.insert_str("idle prompt");
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::UserMessage {
            text: "idle prompt".into(),
            images: Vec::new(),
            message_id: None,
        })]
    );
    assert!(state.input.is_empty());
    assert!(state.input.prompt_queue.is_empty());

    // 2. Enter while a turn runs queues instead of sending, and the prompt clears
    state.turn_active = true;
    state.input.insert_str("queued prompt");
    let actions = handle_key(&mut state, enter, now);
    assert!(actions.is_empty());
    assert!(state.input.is_empty());
    assert_eq!(state.input.prompt_queue.len(), 1);
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("queued prompt")
    );
}

#[test]
fn enter_queues_while_prompt_ack_waits_for_turn_boundary() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.turn_boundary_pending = true;
    state.input.insert_str("next");
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );

    assert!(actions.is_empty());
    assert!(state.input.is_empty());
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("next")
    );
}

#[test]
fn test_queue_drains_in_order_on_turn_finished() {
    let mut state = make_test_state();
    state.meta = Some(pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_queue"),
        name: None,
        cwd: "/tmp".into(),
        git_branch: None,
        model: ModelRoute::new("p", "m"),
        effort: Effort::Medium,
        mode: pacode_types::state::Mode::Build,
        created_at_ms: 1,
        updated_at_ms: 1,
        first_prompt: None,
        zdr: false,
    });
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    let client_context = pacode_client::ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(pacode_types::SessionId::new("ses_queue")),
    };
    let now = Instant::now();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    // Queue two prompts while a turn is active
    state.turn_active = true;
    state.input.insert_str("first");
    handle_key(&mut state, enter, now);
    state.input.insert_str("second");
    handle_key(&mut state, enter, now);
    assert_eq!(state.input.prompt_queue.len(), 2);

    // First turn finishes: TurnEnded event arrives for main agent
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 1,
            event: pacode_types::Event::TurnEnded {
                agent: AgentId::main(),
                turn: pacode_types::TurnId::new("turn_1"),
                usage: None,
                stop: pacode_types::TurnStop::Completed,
            },
        },
        now,
    );

    // The FIRST queued entry is drained
    let req1 = state.drain_prompt_queue();
    assert_eq!(
        req1,
        Some(Request::UserMessage {
            text: "first".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
    assert_eq!(state.input.prompt_queue.len(), 1);
    assert!(!state.turn_active);

    state.user_msgs_in_flight = 1;
    assert_eq!(state.drain_prompt_queue(), None);
    state.user_msgs_in_flight = 0;
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 2,
            event: pacode_types::Event::TurnStarted {
                agent: AgentId::main(),
                turn: pacode_types::TurnId::new("turn_2"),
            },
        },
        now,
    );
    assert!(state.turn_active);
    assert_eq!(state.drain_prompt_queue(), None);

    // Second turn finishes
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 3,
            event: pacode_types::Event::TurnEnded {
                agent: AgentId::main(),
                turn: pacode_types::TurnId::new("turn_2"),
                usage: None,
                stop: pacode_types::TurnStop::Completed,
            },
        },
        now,
    );

    // The SECOND queued entry is drained
    let req2 = state.drain_prompt_queue();
    assert_eq!(
        req2,
        Some(Request::UserMessage {
            text: "second".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
    assert_eq!(state.input.prompt_queue.len(), 0);

    // Third turn finishes, queue is now empty
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 4,
            event: pacode_types::Event::TurnEnded {
                agent: AgentId::main(),
                turn: pacode_types::TurnId::new("turn_3"),
                usage: None,
                stop: pacode_types::TurnStop::Completed,
            },
        },
        now,
    );
    let req3 = state.drain_prompt_queue();
    assert_eq!(req3, None);
    assert!(!state.turn_active);
}

#[test]
fn test_queue_cap_refuses_with_toast_and_preserves_entries() {
    let mut state = make_test_state();
    let now = Instant::now();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    state.turn_active = true;

    // Fill the queue up to PROMPT_QUEUE_CAP
    for i in 0..crate::state::input::PROMPT_QUEUE_CAP {
        state.input.insert_str(&format!("msg {i}"));
        handle_key(&mut state, enter, now);
    }
    assert_eq!(
        state.input.prompt_queue.len(),
        crate::state::input::PROMPT_QUEUE_CAP
    );

    // Now try to queue one more
    state.input.insert_str("overflow prompt");
    let actions = handle_key(&mut state, enter, now);
    assert!(actions.is_empty());
    // Refused with a toast
    assert!(
        state
            .toasts
            .iter()
            .any(|t| t.title.contains("Prompt queue full"))
    );
    // Existing entries not dropped
    assert_eq!(
        state.input.prompt_queue.len(),
        crate::state::input::PROMPT_QUEUE_CAP
    );
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("msg 0")
    );
    // New text is not lost / dropped from the prompt
    assert_eq!(state.input.text, "overflow prompt");
}

#[test]
fn test_removing_and_clearing_queue() {
    let mut state = make_test_state();
    let now = Instant::now();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    state.turn_active = true;

    // Verify bindings exist in Keymap::defaults()
    let keymap = crate::binding::Keymap::defaults();
    assert!(!keymap.bindings_for(KeyAction::RemoveQueued).is_empty());
    assert!(!keymap.bindings_for(KeyAction::ClearQueue).is_empty());

    // Queue 3 entries
    state.input.insert_str("p1");
    handle_key(&mut state, enter, now);
    state.input.insert_str("p2");
    handle_key(&mut state, enter, now);
    state.input.insert_str("p3");
    handle_key(&mut state, enter, now);
    assert_eq!(state.input.prompt_queue.len(), 3);

    // alt+q removes the latest entry
    let alt_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_q, now);
    assert!(actions.is_empty());
    assert_eq!(state.input.prompt_queue.len(), 2);
    assert_eq!(
        state.input.prompt_queue.back().map(String::as_str),
        Some("p2")
    );

    // alt+shift+q clears the entire queue
    let alt_shift_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    let actions = handle_key(&mut state, alt_shift_q, now);
    assert!(actions.is_empty());
    assert!(state.input.prompt_queue.is_empty());
}

#[test]
fn test_ctrl_enter_submit_now_behavior() {
    let mut state = make_test_state();
    let now = Instant::now();
    let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);

    // 1. When idle: ordinary submit
    state.turn_active = false;
    state.input.insert_str("send now when idle");
    let actions = handle_key(&mut state, ctrl_enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::UserMessage {
            text: "send now when idle".into(),
            images: Vec::new(),
            message_id: None,
        })]
    );
    assert!(state.input.is_empty());

    // 2. While a turn runs, the running turn is detached to a background agent
    // first and the new message then starts a fresh turn on main. Order matters:
    // the message must not reach main before the running turn has left it.
    state.turn_active = true;
    state.input.insert_str("send now while busy");
    let actions = handle_key(&mut state, ctrl_enter, now);
    assert_eq!(
        actions,
        vec![Action::SendSequence {
            requests: vec![
                Request::DetachTurn,
                Request::UserMessage {
                    text: "send now while busy".into(),
                    images: Vec::new(),
                    message_id: None,
                },
            ],
        }]
    );
    assert!(state.input.is_empty());
    assert!(
        state.input.prompt_queue.is_empty(),
        "ctrl+enter must bypass the queue, not append to it"
    );
}

#[test]
fn submit_now_respects_an_in_flight_prompt_gate() {
    let mut state = make_test_state();
    state.turn_active = true;
    state.user_msgs_in_flight = 1;
    state.input.insert_str("wait for the gate");
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        Instant::now(),
    );

    assert!(actions.is_empty());
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("wait for the gate")
    );
}

#[test]
fn test_background_completion_renders_a_result_cell_with_exit_code() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Background task completed with non-zero exit code
    let failed_task = pacode_types::TaskInfo {
        id: pacode_types::TaskId::new("task_f1"),
        session: pacode_types::SessionId::new("ses_1"),
        owner: AgentId::main(),
        label: "test run".into(),
        command: "cargo test".into(),
        cwd: std::path::PathBuf::from("/tmp"),
        status: pacode_types::state::TaskStatus::Failed,
        backgrounded: true,
        exit_code: Some(101),
        started_at_ms: 0,
        ended_at_ms: Some(100),
        progress: None,
        warnings: 0,
        errors: 1,
        output_path: std::path::PathBuf::from("/tmp/out"),
        output_bytes: 0,
        acked: false,
    };

    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq: 1,
            event: pacode_types::Event::TaskUpdated(failed_task),
        },
        now,
    );

    let cell = state
        .transcript
        .cells
        .back()
        .expect("background result cell was added");
    let result = match &cell.kind {
        CellKind::BackgroundResult(r) => r.clone(),
        other => panic!("expected a background result cell, got {other:?}"),
    };
    assert_eq!(result.outcome, crate::state::BackgroundOutcome::Failed);
    assert_eq!(result.exit_code, Some(101));
    assert_eq!(result.label, "cargo test");

    // A failure is reported however short it was, and the toast is gone: the
    // transcript line is the single report.
    assert!(state.toasts.is_empty(), "the duplicate toast must be gone");

    let opts = pacode_render::RenderOptions::new(80, false);
    let lines = crate::ui::dialog::render_cell_for_test(cell, &opts);
    assert_eq!(lines.len(), 1);
    let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(rendered.contains("101"), "exit code missing: {rendered}");
    assert!(rendered.contains("cargo test"), "label missing: {rendered}");
    assert!(rendered.contains("failed"), "outcome missing: {rendered}");
}

#[test]
fn test_short_successful_background_task_is_not_reported() {
    let mut state = make_test_state();
    let before = state.transcript.cells.len();

    let quick = pacode_types::TaskInfo {
        id: pacode_types::TaskId::new("task_q1"),
        session: pacode_types::SessionId::new("ses_1"),
        owner: AgentId::main(),
        label: "quick".into(),
        command: "true".into(),
        cwd: std::path::PathBuf::from("/tmp"),
        status: pacode_types::state::TaskStatus::Completed,
        backgrounded: true,
        exit_code: Some(0),
        started_at_ms: pacode_types::time::now_ms(),
        ended_at_ms: Some(pacode_types::time::now_ms() + 200),
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: std::path::PathBuf::from("/tmp/out"),
        output_bytes: 0,
        acked: false,
    };

    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq: 1,
            event: pacode_types::Event::TaskUpdated(quick),
        },
        Instant::now(),
    );

    assert_eq!(state.transcript.cells.len(), before);
    assert!(state.toasts.is_empty());
}

#[test]
fn test_alt_r_opens_the_plan_and_agents_overlay() {
    let mut state = make_test_state();
    let now = Instant::now();

    let alt_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT);
    handle_key(&mut state, alt_r, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay {
            selected: AgentId::main(),
        })
    );

    // Esc closes it again.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_agent_navigation_opens_the_overlay_when_the_rail_is_hidden() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Below 80 columns the rail is not drawn, so there is nothing to navigate.
    state.cols = 70;
    let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
    handle_key(&mut state, alt_down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay {
            selected: AgentId::main(),
        })
    );

    // Wide enough for the rail: navigation selects in the rail as before.
    state.focus = Focus::Normal;
    state.cols = 120;
    handle_key(&mut state, alt_down, now);
    assert!(!matches!(
        state.focus,
        Focus::Overlay(Overlay::RailOverlay { .. })
    ));
}

fn test_question() -> pacode_types::Question {
    pacode_types::Question::new(
        pacode_types::QuestionId::new("qst_1"),
        pacode_types::QuestionOrigin::new(
            AgentId::main(),
            "main",
            pacode_types::CallId::new("call_1"),
        ),
        "Storage",
        "Where should the cache live?",
        vec![
            pacode_types::QuestionOption::new("Cache dir", "the usual place"),
            pacode_types::QuestionOption::new("Next to the project", "portable").recommended(),
        ],
        false,
        0,
    )
    .expect("question")
}

fn ask(state: &mut AppState, question: pacode_types::Question, now: Instant) {
    let seq = state.last_event_seq.map(|seq| seq + 1).unwrap_or(1);
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq,
            event: pacode_types::Event::QuestionAsked(question),
        },
        now,
    );
}

#[test]
fn test_a_question_takes_the_keyboard_and_starts_on_the_recommended_option() {
    let mut state = make_test_state();
    ask(&mut state, test_question(), Instant::now());

    assert!(
        matches!(state.focus, Focus::Overlay(Overlay::QuestionPicker)),
        "expected the question picker, got {:?}",
        state.focus
    );
    assert_eq!(state.questions.focus().map(|f| f.index), Some(1));
    assert!(state.is_bottom_picker());
}

#[test]
fn test_questions_queue_up_and_walk_with_arrows() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    second.question = "And the second?".to_string();
    ask(&mut state, second, now);

    // Both are waiting; the first keeps the cursor.
    assert_eq!(state.questions.len(), 2);
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_1")
    );
    assert_eq!(state.questions.position(), 1);

    // Right/Left walk the queue, wrapping at the ends.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        now,
    );
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_2")
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        now,
    );
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_1")
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        now,
    );
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_2")
    );
}

#[test]
fn test_each_question_keeps_its_own_picker_state() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    ask(&mut state, second, now);

    // Cursor to the plaque on qst_1 (2 options → index 2), then walk.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    assert_eq!(state.questions.focus().map(|f| f.index), Some(2));
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        now,
    );
    assert_eq!(
        state.questions.focus().map(|f| f.index),
        Some(1),
        "qst_2 opens on its own recommended option"
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        now,
    );
    assert_eq!(
        state.questions.focus().map(|f| f.index),
        Some(2),
        "qst_1 kept its plaque cursor"
    );
}

#[test]
fn test_typed_text_survives_walking_the_queue() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    ask(&mut state, second, now);

    // Half-type a custom answer on qst_1.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        now,
    );
    for c in "half".chars() {
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            now,
        );
    }
    // Arrows still walk the queue while typing; the other question shows its
    // own (empty) state…
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        now,
    );
    let f = state.questions.focus().expect("focus");
    assert!(f.typed.is_empty());
    assert!(!f.typing);
    // …and coming back restores the half-written answer, still typing.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        now,
    );
    let f = state.questions.focus().expect("focus");
    assert_eq!(f.typed, "half");
    assert!(f.typing);

    // Enter submits it against qst_1.
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { question, answer })] => {
            assert_eq!(question.as_str(), "qst_1");
            assert_eq!(answer.free_text.as_deref(), Some("half"));
        }
        other => panic!("expected the typed answer, got {other:?}"),
    }
}

#[test]
fn test_resolving_one_question_keeps_the_picker_for_the_rest() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    ask(&mut state, second, now);

    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq: state.last_event_seq.map(|seq| seq + 1).unwrap_or(1),
            event: pacode_types::Event::QuestionResolved {
                question: pacode_types::QuestionId::new("qst_1"),
                answer: pacode_types::QuestionAnswer::choice(0),
            },
        },
        now,
    );
    // One down, one still waiting — the picker re-anchors on it.
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::QuestionPicker)
    ));
    assert_eq!(state.questions.len(), 1);
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_2")
    );
}

#[test]
fn test_answering_one_question_advances_to_the_next() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    ask(&mut state, second, now);

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { question, .. })] => {
            assert_eq!(question.as_str(), "qst_1");
        }
        other => panic!("expected an answer for qst_1, got {other:?}"),
    }
    // The answered question leaves the queue; the picker stays for qst_2.
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::QuestionPicker)
    ));
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_2")
    );
}

#[test]
fn test_submissions_follow_the_queue_cursor_not_a_stale_view() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);
    let mut second = test_question();
    second.id = pacode_types::QuestionId::new("qst_2");
    ask(&mut state, second, now);

    // The user walked to qst_2 — digit and Enter both answer THAT question.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        now,
    );
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { question, answer })] => {
            assert_eq!(question.as_str(), "qst_2");
            assert_eq!(answer.selected, vec![1]);
        }
        other => panic!("expected an answer for qst_2, got {other:?}"),
    }

    // Same guard after a resolve: qst_1 answered elsewhere while a third
    // question waits behind it — the queue re-anchored, so the next Enter
    // goes to what is actually on screen, not the stale view.
    let mut third = test_question();
    third.id = pacode_types::QuestionId::new("qst_3");
    ask(&mut state, third, now);
    // Queue is now [qst_1, qst_3] with the cursor on qst_1.
    assert_eq!(
        state.questions.current().map(|q| q.id.as_str()),
        Some("qst_1")
    );
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq: state.last_event_seq.map(|seq| seq + 1).unwrap_or(1),
            event: pacode_types::Event::QuestionResolved {
                question: pacode_types::QuestionId::new("qst_1"),
                answer: pacode_types::QuestionAnswer::choice(0),
            },
        },
        now,
    );
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { question, .. })] => {
            assert_eq!(
                question.as_str(),
                "qst_3",
                "the resolved question must not take the answer"
            );
        }
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn test_enter_answers_with_the_option_under_the_cursor() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    assert_eq!(state.focus, Focus::Normal);
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { question, answer })] => {
            assert_eq!(question.as_str(), "qst_1");
            assert_eq!(answer.selected, vec![1]);
            assert!(!answer.cancelled);
        }
        other => panic!("expected one answer action, got {other:?}"),
    }
}

#[test]
fn test_a_digit_picks_that_option_directly() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { answer, .. })] => {
            assert_eq!(answer.selected, vec![0]);
        }
        other => panic!("expected an answer, got {other:?}"),
    }

    // A digit past the last option does nothing rather than answering wrongly.
    let mut state = make_test_state();
    ask(&mut state, test_question(), now);
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::QuestionPicker)
    ));
}

#[test]
fn test_esc_dismisses_the_question_rather_than_leaving_the_turn_waiting() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { answer, .. })] => assert!(answer.cancelled),
        other => panic!("dismissing must answer, got {other:?}"),
    }
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_typing_an_answer_of_your_own() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        now,
    );
    for c in "elsewhere".chars() {
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            now,
        );
    }
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { answer, .. })] => {
            assert_eq!(answer.free_text.as_deref(), Some("elsewhere"));
            assert!(answer.selected.is_empty());
        }
        other => panic!("expected the typed answer, got {other:?}"),
    }
}

#[test]
fn test_multi_select_toggles_with_space() {
    let mut state = make_test_state();
    let now = Instant::now();
    let mut q = test_question();
    q.multi_select = true;
    ask(&mut state, q, now);

    // Cursor starts on the recommended option; toggle it and the one above.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        now,
    );
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::AnswerQuestion { answer, .. })] => {
            assert_eq!(answer.selected, vec![1, 0]);
        }
        other => panic!("expected both options, got {other:?}"),
    }
}

#[test]
fn test_the_free_text_plaque_is_a_pseudo_option_past_the_last() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    // j past the last option lands on the plaque (index == count).
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    assert_eq!(state.questions.focus().map(|f| f.index), Some(2));
    // …and stays there on another j.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    assert_eq!(state.questions.focus().map(|f| f.index), Some(2));

    // Enter on the plaque does not answer — it starts typing.
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert_eq!(state.questions.focus().map(|f| f.typing), Some(true));
}

#[test]
fn test_digit_past_the_options_jumps_to_the_plaque() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    // `3` is N+1 for two options: it moves to the plaque, it does not answer.
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert_eq!(state.questions.focus().map(|f| f.index), Some(2));
}

#[test]
fn test_a_letter_on_the_plaque_starts_typing_immediately() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    // Reach the plaque (index == count), then a plain letter types it.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        now,
    );
    let f = state.questions.focus().expect("focus");
    assert!(f.typing);
    assert_eq!(f.typed, "x");

    // Nav letters still navigate while parked on the plaque: 'k' steps back
    // to the last option rather than typing a 'k'.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        now,
    );
    let f = state.questions.focus().expect("focus");
    assert!(!f.typing);
    assert_eq!(f.index, 1);
}

#[test]
fn test_esc_from_typing_returns_to_the_list() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        now,
    );
    for c in "half".chars() {
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            now,
        );
    }
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    let f = state.questions.focus().expect("focus");
    assert!(!f.typing);
    assert!(f.typed.is_empty());
}

#[test]
fn test_space_on_the_plaque_starts_typing_even_in_multi_select() {
    let mut state = make_test_state();
    let now = Instant::now();
    let mut q = test_question();
    q.multi_select = true;
    ask(&mut state, q, now);

    // Cursor starts on the recommended option; move to the plaque, then space.
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        now,
    );
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        now,
    );
    let f = state.questions.focus().expect("focus");
    assert!(f.typing);
    assert!(f.selected.is_empty());
}

#[test]
fn test_question_picker_height_tracks_options_descriptions_and_cap() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    // cols = 120 → dialog column 86 wide: chip + question + blank + 2×2
    // option rows + plaque block + hint = 12.
    assert_eq!(state.bottom_picker_height(), 12);

    // Six options with descriptions overflow the cap.
    let mut state = make_test_state();
    let big = pacode_types::Question::new(
        pacode_types::QuestionId::new("qst_big"),
        pacode_types::QuestionOrigin::new(
            AgentId::main(),
            "main",
            pacode_types::CallId::new("call_2"),
        ),
        "Pick",
        "Which six?",
        (0..6)
            .map(|i| pacode_types::QuestionOption::new(format!("Option {i}"), format!("d{i}")))
            .collect(),
        false,
        0,
    )
    .expect("question");
    ask(&mut state, big, now);
    assert_eq!(state.bottom_picker_height(), 18);
}

#[test]
fn test_resolving_the_question_elsewhere_closes_the_picker() {
    let mut state = make_test_state();
    let now = Instant::now();
    ask(&mut state, test_question(), now);

    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: pacode_client::ClientEventContext::unscoped(),
            seq: state.last_event_seq.map(|seq| seq + 1).unwrap_or(1),
            event: pacode_types::Event::QuestionResolved {
                question: pacode_types::QuestionId::new("qst_1"),
                answer: pacode_types::QuestionAnswer::choice(0),
            },
        },
        now,
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_login_picker_keys_filtering_selection_and_actions() {
    let mut state = make_test_state();
    let now = Instant::now();

    let p1 = pacode_types::ProviderAuthInfo {
        id: "devin".into(),
        display_name: "Devin".into(),
        auth_kind: "oauth".into(),
        detail: "Autonomous AI software engineer".into(),
        recommended: true,
        state: pacode_types::AuthState::Configured,
        accounts: vec!["acc1".into(), "acc2".into()],
        active: Some("acc1".into()),
    };
    let p2 = pacode_types::ProviderAuthInfo {
        id: "anthropic".into(),
        display_name: "Anthropic Claude".into(),
        auth_kind: "oauth".into(),
        detail: "Claude 3.5 Sonnet & Haiku".into(),
        recommended: false,
        state: pacode_types::AuthState::NotConfigured,
        accounts: vec![],
        active: None,
    };
    let p3 = pacode_types::ProviderAuthInfo {
        id: "openai".into(),
        display_name: "OpenAI".into(),
        auth_kind: "api_key".into(),
        detail: "GPT-4o & o1".into(),
        recommended: false,
        state: pacode_types::AuthState::NeedsAttention {
            reason: "API key expired".into(),
        },
        accounts: vec!["primary".into()],
        active: Some("primary".into()),
    };

    state.auth_providers = vec![p1, p2, p3];

    // Open login picker
    state.focus = Focus::Overlay(Overlay::LoginPicker {
        query: String::new(),
        index: 0,
    });

    // 1. Down moves index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 1,
        })
    );

    // 2. Ctrl+N moves index to 2
    let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_n, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 2,
        })
    );

    // 3. Ctrl+P moves index back to 1
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_p, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 1,
        })
    );

    // 4. Up moves index back to 0 (devin)
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 0,
        })
    );

    // 5. Account switching with 'a': devin has 2 accounts ["acc1", "acc2"], active is "acc1"
    let key_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_a, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::SetAuthAccount {
            provider: "devin".into(),
            label: "acc2".into(),
        })]
    );
    // Query remains empty because 'a' cycled accounts
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 0,
        })
    );

    // 6. Ctrl+D on devin sends Request::Logout for active account "acc1" and does NOT quit
    let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_d, now);
    assert!(!state.quit, "ctrl+d in login picker must not quit app");
    assert_eq!(
        actions,
        vec![Action::Send(Request::Logout {
            provider: "devin".into(),
            label: Some("acc1".into()),
        })]
    );

    // 7. Filtering: move to anthropic, press 'o' to filter
    let char_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
    handle_key(&mut state, char_o, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: "o".into(),
            index: 0,
        })
    );

    // Backspace clears query
    let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
    handle_key(&mut state, backspace, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: String::new(),
            index: 0,
        })
    );

    // 8. Enter sends Request::Login for selected provider (devin)
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(
        actions,
        vec![Action::Send(Request::Login {
            provider: "devin".into(),
        })]
    );

    // 9. Esc closes login picker
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_login_picker_a_key_types_when_single_or_no_accounts() {
    let mut state = make_test_state();
    let now = Instant::now();

    let p1 = pacode_types::ProviderAuthInfo {
        id: "anthropic".into(),
        display_name: "Anthropic Claude".into(),
        auth_kind: "oauth".into(),
        detail: "Claude 3.5 Sonnet & Haiku".into(),
        recommended: false,
        state: pacode_types::AuthState::NotConfigured,
        accounts: vec![],
        active: None,
    };
    state.auth_providers = vec![p1];

    state.focus = Focus::Overlay(Overlay::LoginPicker {
        query: String::new(),
        index: 0,
    });

    // Press 'a': since accounts is empty, it types 'a' into the query
    let key_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_a, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::LoginPicker {
            query: "a".into(),
            index: 0,
        })
    );
}

// ---- Alt+Z session ZDR toggle (spec §3) ----

/// Point `state.paths` at a temp dir and drop any model flags the real
/// `prefs.toml` loaded — picker toggles persist prefs, tests must stay hermetic.
fn isolate_prefs(state: &mut AppState) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    state.paths = pacode_config::Paths::under(dir.path());
    state.model_flags.clear();
    dir
}

fn zdr_meta(model: &str, zdr: bool) -> pacode_types::SessionMeta {
    pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_zdr"),
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        model: ModelRoute::new("p", model),
        effort: Effort::Medium,
        mode: pacode_types::Mode::Build,
        created_at_ms: 0,
        updated_at_ms: 0,
        name: None,
        first_prompt: None,
        zdr,
    }
}

#[test]
fn test_alt_z_toggles_zdr_when_model_capable() {
    let mut state = make_test_state(); // Config::default() has zdr = "session"
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.model_flags.insert(
        "p/m".to_string(),
        pacode_types::ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );

    let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_z, now);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Send(Request::SetZdr(true)))),
        "expected SetZdr(true), got {actions:?}"
    );
    assert!(state.meta.as_ref().is_some_and(|m| m.zdr));

    // Second Alt+Z turns it back off.
    let actions = handle_key(&mut state, alt_z, now);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Send(Request::SetZdr(false)))),
        "expected SetZdr(false), got {actions:?}"
    );
    assert!(state.meta.as_ref().is_some_and(|m| !m.zdr));
}

#[test]
fn test_alt_z_opens_picker_when_model_not_capable() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false)); // no flag => not zdr_capable

    let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_z, now);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Send(Request::ListModels))),
        "expected ListModels, got {actions:?}"
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: String::new(),
            index: 0,
            for_zdr: true,
        })
    );
    assert!(
        !state.meta.as_ref().is_some_and(|m| m.zdr),
        "zdr stays off until a capable model is picked"
    );
}

#[test]
fn test_alt_z_noop_when_zdr_config_off() {
    let mut state = make_test_state();
    state.config.zdr = pacode_types::ZdrMode::Off;
    let now = Instant::now();
    state.meta = Some(zdr_meta("m", false));

    let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_z, now);
    assert!(actions.is_empty(), "zdr=off must ignore Alt+Z");
    assert_eq!(state.focus, Focus::Normal);
}

fn model_info(provider: &str, model: &str) -> pacode_types::model::ModelInfo {
    pacode_types::model::ModelInfo {
        route: ModelRoute::new(provider, model),
        display_name: model.to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }
}

#[test]
fn test_model_picker_query_can_start_with_s() {
    // §9.1: letters always reach the query — "sonnet" types out whole.
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.models = vec![model_info("p", "sonnet"), model_info("p", "m")];
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
        for_zdr: false,
    });

    for (i, c) in "sonnet".chars().enumerate() {
        let actions = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            now,
        );
        assert!(
            actions.is_empty(),
            "letter '{c}' must type, not toggle: {actions:?}"
        );
        let typed: String = "sonnet".chars().take(i + 1).collect();
        assert_eq!(
            state.focus,
            Focus::Overlay(Overlay::ModelPicker {
                query: typed.clone(),
                index: 0,
                for_zdr: false,
            }),
            "after '{typed}'"
        );
    }
    assert!(
        state.model_flags.is_empty(),
        "plain letters never touch the marks"
    );
}

#[test]
fn test_model_picker_mark_chords_toggle_flags() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.models = vec![model_info("p", "m")];
    // Regular picker (zdr inactive): the full list is visible, so the ZDR
    // chord can bootstrap zdr_capable on a row — spec's marking flow.
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
        for_zdr: false,
    });

    let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_z, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/m" && flags.zdr_capable
        )),
        "expected SetModelFlag zdr_capable, got {actions:?}"
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: String::new(),
            index: 0,
            for_zdr: false,
        }),
        "alt+z must not leak into the query"
    );
    assert!(
        state.model_flags.get("p/m").is_some_and(|f| f.zdr_capable),
        "state.model_flags must reflect the toggle"
    );

    // alt+f toggles favorite the same way.
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_f, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/m" && flags.favorite && flags.zdr_capable
        )),
        "expected SetModelFlag favorite, got {actions:?}"
    );

    // The ctrl fallbacks hit the same flags (alt-eating terminals).
    let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_y, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/m" && !flags.zdr_capable && flags.favorite
        )),
        "ctrl+y must toggle zdr_capable back off, got {actions:?}"
    );
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_g, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/m" && !flags.favorite
        )),
        "ctrl+g must toggle favorite back off, got {actions:?}"
    );

    // Enter picks the row: only SetModel (for_zdr is false).
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions.len(), 1, "expected just SetModel, got {actions:?}");
    assert!(
        matches!(&actions[0], Action::Send(Request::SetModel(r)) if r.to_string() == "p/m"),
        "expected SetModel, got {actions:?}"
    );
    assert!(state.meta.as_ref().is_some_and(|m| !m.zdr));
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_model_picker_mark_chords_work_mid_query_and_rebind() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.models = vec![model_info("p", "sonnet")];
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: "son".to_string(),
        index: 0,
        for_zdr: false,
    });

    // Marks fire on the filtered row even after typing (§9.1).
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_g, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/sonnet" && flags.favorite
        )),
        "expected SetModelFlag favorite on the filtered row, got {actions:?}"
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: "son".to_string(),
            index: 0,
            for_zdr: false,
        }),
        "the mark chord must not touch the query"
    );

    // A rebind through the keymap moves the mark: the new chord toggles,
    // the old factory chords stop firing for this action.
    state.keymap.set_bindings(
        crate::binding::Action::ModelPickerFavorite,
        vec![crate::binding::Binding {
            code: KeyCode::Char('m'),
            mods: KeyModifiers::CONTROL,
        }],
    );
    let actions = handle_key(&mut state, ctrl_g, now);
    assert!(
        actions.is_empty(),
        "the unbound fallback must go quiet, got {actions:?}"
    );
    let ctrl_m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);
    let actions = handle_key(&mut state, ctrl_m, now);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Request::SetModelFlag { model, flags })
                if model == "p/sonnet" && !flags.favorite
        )),
        "the rebound chord must toggle, got {actions:?}"
    );
}

#[test]
fn test_model_picker_for_zdr_enter_sends_setmodel_then_setzdr() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.models = vec![model_info("p", "m"), model_info("p", "other")];
    // Only "p/m" is capable — the for_zdr list filters "p/other" out.
    state.model_flags.insert(
        "p/m".to_string(),
        pacode_types::ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
        for_zdr: true,
    });

    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions.len(), 2, "expected [SetModel, SetZdr]");
    assert!(
        matches!(&actions[0], Action::Send(Request::SetModel(r)) if r.to_string() == "p/m"),
        "first action must be SetModel, got {actions:?}"
    );
    assert!(
        matches!(&actions[1], Action::Send(Request::SetZdr(true))),
        "second action must be SetZdr(true), got {actions:?}"
    );
    assert!(state.meta.as_ref().is_some_and(|m| m.zdr));
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_model_picker_z_s_always_type_into_query() {
    // §9.1: 'z'/'s' are search text in every query state, not flag toggles.
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    state.models = vec![model_info("p", "m")];
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: "mi".to_string(),
        index: 0,
        for_zdr: false,
    });

    let key_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_z, now);
    assert!(
        actions.is_empty() && state.model_flags.is_empty(),
        "'z' inside a query must not toggle flags, got {actions:?}"
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: "miz".to_string(),
            index: 0,
            for_zdr: false,
        })
    );

    let key_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    let actions = handle_key(&mut state, key_s, now);
    assert!(actions.is_empty() && state.model_flags.is_empty());
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker {
            query: "mizs".to_string(),
            index: 0,
            for_zdr: false,
        })
    );
}

#[test]
fn test_model_picker_z_refuses_unmarking_active_route_under_zdr() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", true)); // ZDR on, route p/m
    state.models = vec![model_info("p", "m")];
    state.model_flags.insert(
        "p/m".to_string(),
        pacode_types::ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
        for_zdr: false,
    });

    // The daemon would reject stripping the active route's flag; the picker
    // refuses locally so state can't diverge. Both spellings of the mark.
    for key in [
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    ] {
        let actions = handle_key(&mut state, key, now);
        assert!(
            actions.is_empty(),
            "unmark must be refused for {key:?}, got {actions:?}"
        );
    }
    assert!(
        state.model_flags.get("p/m").is_some_and(|f| f.zdr_capable),
        "flag must survive the refused toggle"
    );
}

#[test]
fn test_effort_picker_refuses_unsupported() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    state.meta = Some(zdr_meta("m", false));
    let mut mi = model_info("p", "m");
    mi.supported_efforts = Some(vec![Effort::Low, Effort::Medium]);
    state.models = vec![mi];

    // index 2 = High, unsupported by p/m -> refused, picker stays open.
    state.focus = Focus::Overlay(Overlay::EffortPicker { index: 2 });
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert!(actions.is_empty(), "unsupported effort must not send");
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::EffortPicker { index: 2 }),
        "refused pick keeps the picker open"
    );

    // index 0 = Low, supported -> sends and closes.
    state.focus = Focus::Overlay(Overlay::EffortPicker { index: 0 });
    let actions = handle_key(&mut state, enter, now);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Send(Request::SetEffort(Effort::Low)))),
        "expected SetEffort(Low), got {actions:?}"
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_model_switch_clamps_unsupported_effort() {
    let mut state = make_test_state();
    let now = Instant::now();
    let _tmp = isolate_prefs(&mut state);
    let mut meta = zdr_meta("m", false);
    meta.effort = Effort::High;
    state.meta = Some(meta);
    let mut target = model_info("p", "n2");
    target.supported_efforts = Some(vec![Effort::Low, Effort::Medium]);
    state.models = vec![target];
    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: String::new(),
        index: 0,
        for_zdr: false,
    });

    // High isn't in p/n2's supported set -> nearest lower supported = Medium.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions.len(), 2, "expected [SetModel, SetEffort]");
    assert!(
        matches!(&actions[0], Action::Send(Request::SetModel(r)) if r.to_string() == "p/n2"),
        "first action must be SetModel, got {actions:?}"
    );
    assert!(
        matches!(
            &actions[1],
            Action::Send(Request::SetEffort(Effort::Medium))
        ),
        "second action must be the downgrade to Medium, got {actions:?}"
    );
}

fn permission_cell(id: u64, perm_id: &str) -> crate::state::Cell {
    crate::state::Cell {
        id,
        kind: CellKind::Item(TranscriptKind::Permission(
            pacode_types::PermissionRequest {
                id: pacode_types::PermissionId::new(perm_id),
                agent: AgentId::main(),
                agent_name: "main".into(),
                call_id: pacode_types::CallId::new("call_1"),
                tool: "bash".into(),
                title: "Bash: rm -rf /tmp/x".into(),
                detail: String::new(),
                risk: Some(pacode_types::RiskLevel::Confirm),
                created_at_ms: 1,
            },
        )),
        version: 0,
        ts_ms: 0,
        stats: None,
        perm_explain: None,
    }
}

#[test]
fn test_e_on_a_pending_permission_asks_for_the_explanation() {
    let mut state = make_test_state();
    let now = Instant::now();
    state
        .transcript
        .cells
        .push_back(permission_cell(1, "perm_1"));

    // 'e' asks the daemon's detached explainer — it never resolves the
    // request. The prompt flips to the in-flight mark so the press is
    // visibly acknowledged.
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [Action::Send(Request::ExplainPermission { permission })] => {
            assert_eq!(permission.as_str(), "perm_1");
        }
        other => panic!("expected ExplainPermission, got {other:?}"),
    }
    assert_eq!(
        state.transcript.cells[0].perm_explain,
        Some(crate::state::PermExplain::Requested)
    );
    assert!(
        state.input.is_empty(),
        "'e' on the prompt must not type into the input"
    );

    // The verdict keys stay armed while the answer is in flight.
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        now,
    );
    match actions.as_slice() {
        [
            Action::Send(Request::PermissionReply {
                permission,
                decision,
                ..
            }),
        ] => {
            assert_eq!(permission.as_str(), "perm_1");
            assert_eq!(
                *decision,
                pacode_types::state::PermissionDecision::AllowOnce
            );
        }
        other => panic!("expected PermissionReply, got {other:?}"),
    }
}

#[test]
fn test_e_with_text_in_the_prompt_types_instead_of_explaining() {
    let mut state = make_test_state();
    let now = Instant::now();
    state
        .transcript
        .cells
        .push_back(permission_cell(1, "perm_1"));
    state.input.insert_str("not");

    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "note");
    assert_eq!(state.transcript.cells[0].perm_explain, None);
}

#[test]
fn test_e_without_a_pending_permission_types_into_the_prompt() {
    let mut state = make_test_state();
    let now = Instant::now();
    let actions = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        now,
    );
    assert!(actions.is_empty());
    assert_eq!(state.input.text, "e");
}
