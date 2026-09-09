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

    // 4. Alt+F -> Follow
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_f, now);
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

    // Empty prompt: '.' opens BgList
    let dot = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE);
    handle_key(&mut state, dot, now);
    assert_eq!(state.focus, Focus::BgList { index: 0 });

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
fn test_overlay_model_picker_navigation_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();
    state.models = vec![
        pacode_types::ModelInfo {
            route: ModelRoute::new("p", "m1"),
            display_name: "Model 1".into(),
            context_window: Some(1000),
            supports_reasoning: true,
            pricing: None,
        },
        pacode_types::ModelInfo {
            route: ModelRoute::new("p", "m2"),
            display_name: "Model 2".into(),
            context_window: Some(1000),
            supports_reasoning: true,
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
fn test_overlay_config_picker_navigation_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();

    // Open via slash command `/config` + Enter
    state.input.insert_str("/config");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ConfigPicker {
            index: 0,
            editing_number: None,
        })
    );

    // Down key changes index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::ConfigPicker {
            index: 1,
            editing_number: None,
        })
    );

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
    };
    state.sessions = vec![meta1, meta2];

    // Open via slash command `/sessions` + Enter
    state.input.insert_str("/sessions");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 0,
        })
    );

    // Keys do NOT leak to input!
    let char_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    handle_key(&mut state, char_a, now);
    assert!(state.input.is_empty());
    // 'a' went to query
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::SessionPicker {
            query: "a".into(),
            index: 0,
        })
    );

    // Backspace clears query
    let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
    handle_key(&mut state, backspace, now);

    // Down key changes index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 1,
        })
    );

    // Esc closes it
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // Open via ctrl+p
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_p, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 0,
        })
    );
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::SessionPicker {
            query: String::new(),
            index: 1,
        })
    );
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_files_alt_b_navigation_enter_and_esc() {
    let mut state = make_test_state();
    let now = Instant::now();

    state
        .files
        .observe_tool_item("read", &serde_json::json!({"path": "src/first.rs"}), 100);
    state
        .files
        .observe_tool_item("write", &serde_json::json!({"path": "src/second.rs"}), 200);

    // 1. alt+b opens Overlay::Files
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    handle_key(&mut state, alt_b, now);
    assert_eq!(state.focus, Focus::Overlay(Overlay::Files { index: 0 }));

    // 2. Down key increments index
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(state.focus, Focus::Overlay(Overlay::Files { index: 1 }));

    // 3. Typing letters does NOT leak to input
    let char_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    handle_key(&mut state, char_z, now);
    assert!(state.input.is_empty());

    // 4. Up key decrements index
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert_eq!(state.focus, Focus::Overlay(Overlay::Files { index: 0 }));

    // 5. Enter copies selected path (second.rs has newer ts 200, so sorted index 0 is second.rs)
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.input.text, "src/second.rs");

    // 6. Reopen with alt+b and press Esc to close
    handle_key(&mut state, alt_b, now);
    assert_eq!(state.focus, Focus::Overlay(Overlay::Files { index: 0 }));
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // 7. Verify alt+f is follow (does not open files)
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    handle_key(&mut state, alt_f, now);
    assert_ne!(state.focus, Focus::Overlay(Overlay::Files { index: 0 }));
}

#[test]
fn test_alt_f_toggles_follow() {
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

    // 1. alt+f toggles follow to true
    let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_f, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id.clone()),
            follow: true,
            follow_paused: false,
        }
    );

    // 2. alt+f toggles follow back to false
    let actions = handle_key(&mut state, alt_f, now);
    assert!(actions.is_empty());
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id.clone()),
            follow: false,
            follow_paused: false,
        }
    );

    // 3. alt+f on SelectAgent for a subagent opens panel with follow = true
    state.focus = Focus::SelectAgent { index: 1 };
    let actions = handle_key(&mut state, alt_f, now);
    assert_eq!(actions, vec![Action::LoadPanel]);
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::Agent(subagent_id),
            follow: true,
            follow_paused: false,
        }
    );

    // 4. alt+f on SelectAgent for main returns to Focus::Normal without opening panel
    state.focus = Focus::SelectAgent { index: 0 };
    let actions = handle_key(&mut state, alt_f, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.panel.target, None);
}

#[test]
fn test_alt_b_opens_files_overlay() {
    let mut state = make_test_state();
    let now = Instant::now();

    assert_eq!(state.focus, Focus::Normal);
    let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_b, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Overlay(Overlay::Files { index: 0 }));
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
    state.focus = Focus::Overlay(Overlay::RailOverlay);
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    // Still focused
    assert_eq!(state.focus, Focus::Overlay(Overlay::RailOverlay));
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);

    // Help via slash command `/help` + Enter
    state.input.insert_str("/help");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter, now);
    assert_eq!(state.focus, Focus::Overlay(Overlay::Help));

    handle_key(&mut state, down, now);
    // Still focused
    assert_eq!(state.focus, Focus::Overlay(Overlay::Help));
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_overlay_mcp_picker_keys() {
    let mut state = make_test_state();
    let now = Instant::now();

    // 1. Open via slash command `/mcp` + Enter
    state.input.insert_str("/mcp");
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = handle_key(&mut state, enter, now);
    assert_eq!(actions, vec![Action::Send(Request::ListMcpServers)]);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker {
            index: 0,
            servers: Vec::new(),
            loading: true,
        })
    );

    // 2. Populate servers
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
    state.focus = Focus::Overlay(Overlay::McpPicker {
        index: 0,
        servers: vec![server1, server2],
        loading: false,
    });

    // 3. Down moves index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    let actions = handle_key(&mut state, down, now);
    assert!(actions.is_empty());
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker { index: 1, .. })
    ));

    // Typing letters doesn't leak into input
    let char_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    handle_key(&mut state, char_z, now);
    assert!(state.input.is_empty());

    // 4. Up moves index back to 0
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up, now);
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker { index: 0, .. })
    ));

    // 5. 'r' on index 0 (srv1) sends RestartMcpServer + ListMcpServers
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
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker { loading: true, .. })
    ));

    // 6. Enter on index 0 (srv1, status "ready") toggles to disabled (enabled = false)
    if let Focus::Overlay(Overlay::McpPicker {
        ref mut loading, ..
    }) = state.focus
    {
        *loading = false;
    }
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

    // 7. Enter on index 1 (srv2, status "disabled") toggles to enabled (enabled = true)
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

    // 8. Esc closes picker
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
    assert_eq!(actions, vec![Action::Send(Request::ListPlugins)]);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::PluginsPicker {
            index: 0,
            plugins: Vec::new(),
        })
    );

    // 2. Populate plugins
    let p1 = pacode_types::PluginInfo {
        name: "plug1".into(),
        version: "1.0.0".into(),
        kind: "lua".into(),
        tools: vec![],
        commands: vec!["c1".into()],
        error: None,
    };
    let p2 = pacode_types::PluginInfo {
        name: "plug2".into(),
        version: "2.0.0".into(),
        kind: "wasm".into(),
        tools: vec![],
        commands: vec![],
        error: None,
    };
    state.focus = Focus::Overlay(Overlay::PluginsPicker {
        index: 0,
        plugins: vec![p1.clone(), p2.clone()],
    });

    // 3. Down moves index to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down, now);
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::PluginsPicker {
            index: 1,
            plugins: vec![p1, p2],
        })
    );

    // Letters do not leak to input
    let char_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    handle_key(&mut state, char_a, now);
    assert!(state.input.is_empty());

    // 4. Esc closes picker
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc, now);
    assert_eq!(state.focus, Focus::Normal);
}
