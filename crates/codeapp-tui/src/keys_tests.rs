use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

use codeapp_types::model::{Effort, ModelRoute};
use codeapp_types::state::{AgentKind, AgentStatus};
use codeapp_types::{AgentId, AgentInfo, Config};

use super::*;
use crate::state::AppState;

fn make_test_state() -> AppState {
    let config = Config::default();
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
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

    // 2. Alt+Down -> SelectAgent
    let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
    let actions = handle_key(&mut state, alt_down, now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::SelectAgent { index: 0 });

    // 3. Enter on empty prompt -> Panel
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
