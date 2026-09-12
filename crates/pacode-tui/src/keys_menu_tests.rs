//! §3.4/§3.5: stack depth, esc/ctrl+c semantics, cursor/filter restoration,
//! the legacy bridge, direct-vs-hub entries and the double-Shift path.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};
use std::time::Instant;

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus};
use pacode_types::{AgentId, AgentInfo, Config};

use super::*;
use crate::keys::handle_key;
use crate::state::AppState;
use crate::ui::menu::MenuId;

fn make_test_state() -> AppState {
    let config = Config::default();
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
    state.rail.upsert_agent(AgentInfo {
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
    });
    state
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn menu_parts(state: &AppState) -> (&MenuStack, &MenuUiState) {
    let Focus::Overlay(Overlay::Menu { stack, ui }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    (stack, ui)
}

#[test]
fn direct_entry_is_a_one_level_stack() {
    let mut state = make_test_state();
    open(&mut state, MenuId::Help);
    let (stack, _ui) = menu_parts(&state);
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
}

#[test]
fn hub_entry_pushes_a_second_level() {
    let mut state = make_test_state();
    open(&mut state, MenuId::Root);
    // Enter on the Help row — it has a provider, so the stack grows.
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState {
        cursor: RowId::new("help"),
        ..MenuUiState::default()
    };
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
    // The hub's cursor was saved into its level.
    assert_eq!(stack.levels()[0].cursor, RowId::new("help"));
    // The fresh level starts on its own first row.
    assert!(ui.cursor.is_empty() || ui.cursor != RowId::new("help"));
}

#[test]
fn esc_pops_one_level_then_closes() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState::default();
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('?')));
    assert_eq!(stack.depth(), 2);
    // esc — back to the hub.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Esc));
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Root));
    // esc again — the last level pops into "closed": an empty stack.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Esc));
    assert!(stack.is_empty());
}

#[test]
fn esc_restores_cursor_and_filter_of_the_parent() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState {
        cursor: RowId::new("git"),
        query: "abc".to_string(),
        ..MenuUiState::default()
    };
    // Descend — the hub's cursor and query are saved.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Git));
    // Change the child's UI state.
    ui.query = "zzz".to_string();
    ui.cursor = RowId::new("branch");
    // esc — the parent's UI state comes back untouched.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Esc));
    assert_eq!(ui.cursor, RowId::new("git"));
    assert_eq!(ui.query, "abc");
}

#[test]
fn ctrl_c_closes_the_whole_stack() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState::default();
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('?')));
    assert_eq!(stack.depth(), 2);
    handle_menu_key(&mut state, &mut stack, &mut ui, ctrl('c'));
    assert!(stack.is_empty());
}

#[test]
fn an_unported_section_bridges_to_its_legacy_overlay() {
    let mut state = make_test_state();
    // `enter` on the hub's Settings row — no provider yet → ConfigPicker.
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState {
        cursor: RowId::new("settings"),
        ..MenuUiState::default()
    };
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert_eq!(state.focus, Focus::Overlay(Overlay::ConfigPicker));

    // Direct entry to an unported section lands on the legacy overlay too.
    state.focus = Focus::Normal;
    open(&mut state, MenuId::Mcp);
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker { .. })
    ));
}

#[test]
fn arrows_move_the_cursor_over_selectable_rows() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState::default();
    // First selectable row is "settings".
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Down));
    assert_eq!(ui.cursor, RowId::new("plugins"));
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Up));
    assert_eq!(ui.cursor, RowId::new("settings"));
    // Up past the top clamps.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Up));
    assert_eq!(ui.cursor, RowId::new("settings"));
}

#[test]
fn a_filtered_menu_types_into_the_query_and_enters_the_match() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = MenuUiState::default();
    // Type "mcp" — the local filter narrows the command rows.
    for c in "mcp".chars() {
        handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char(c)));
    }
    assert_eq!(ui.query, "mcp");
    let view = menu::view_for(&stack, &state, &ui);
    // `N of M`: M is the pre-filter selectable count, N the kept rows.
    let kept_selectable = view.rows.iter().filter(|r| r.is_selectable()).count();
    assert!(
        view.filter_total
            .is_some_and(|total| total > kept_selectable)
    );
    assert!(
        view.rows
            .iter()
            .any(|r| r.id().is_some_and(|i| i.as_str() == "cmd:mcp"))
    );
    assert!(
        view.rows
            .iter()
            .all(|r| !r.is_selectable() || r.matches("mcp"))
    );
    // Enter runs the matched command row — `/mcp` opens the legacy picker.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::McpPicker { .. })
    ));
}

#[test]
fn ctrl_comma_opens_the_hub() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, ctrl(','), now);
    let (stack, _) = menu_parts(&state);
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Root));
}

#[test]
fn esc_through_handle_key_closes_the_menu() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, ctrl(','), now);
    assert!(matches!(state.focus, Focus::Overlay(Overlay::Menu { .. })));
    handle_key(&mut state, key(KeyCode::Esc), now);
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn ctrl_c_through_handle_key_closes_the_menu() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, ctrl(','), now);
    let actions = handle_key(&mut state, ctrl('c'), now);
    assert!(actions.is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn release_and_repeat_events_never_reach_the_menu() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, ctrl(','), now);
    // A Release/Repeat of Down must not move the cursor.
    let before = menu_parts(&state).1.cursor.clone();
    let rel = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Release);
    let rep = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat);
    handle_key(&mut state, rel, now);
    handle_key(&mut state, rep, now);
    assert_eq!(menu_parts(&state).1.cursor, before);
    // The Press still works.
    handle_key(&mut state, key(KeyCode::Down), now);
    assert_eq!(menu_parts(&state).1.cursor, RowId::new("plugins"));
}

#[test]
fn release_and_repeat_never_duplicate_input() {
    let mut state = make_test_state();
    let now = Instant::now();
    let press = key(KeyCode::Char('a'));
    let repeat =
        KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
    let release = KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    handle_key(&mut state, press, now);
    handle_key(&mut state, repeat, now);
    handle_key(&mut state, release, now);
    assert_eq!(state.input.text, "a");
}

fn shift_press() -> KeyEvent {
    KeyEvent::new_with_kind(
        KeyCode::Modifier(ModifierKeyCode::LeftShift),
        KeyModifiers::SHIFT,
        KeyEventKind::Press,
    )
}

#[test]
fn double_shift_opens_the_hub() {
    let mut state = make_test_state();
    let t = Instant::now();
    handle_key(&mut state, shift_press(), t);
    assert_eq!(state.focus, Focus::Normal);
    handle_key(
        &mut state,
        shift_press(),
        t + std::time::Duration::from_millis(300),
    );
    let (stack, _) = menu_parts(&state);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Root));
}

#[test]
fn double_shift_past_the_threshold_does_not_fire() {
    let mut state = make_test_state();
    let t = Instant::now();
    handle_key(&mut state, shift_press(), t);
    handle_key(
        &mut state,
        shift_press(),
        t + std::time::Duration::from_millis(500),
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn a_key_between_the_taps_breaks_the_pair() {
    let mut state = make_test_state();
    let t = Instant::now();
    handle_key(&mut state, shift_press(), t);
    handle_key(
        &mut state,
        key(KeyCode::Char('a')),
        t + std::time::Duration::from_millis(10),
    );
    handle_key(
        &mut state,
        shift_press(),
        t + std::time::Duration::from_millis(20),
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn double_shift_is_inert_outside_normal_focus() {
    let mut state = make_test_state();
    let t = Instant::now();
    open(&mut state, MenuId::Root);
    handle_key(&mut state, shift_press(), t);
    handle_key(
        &mut state,
        shift_press(),
        t + std::time::Duration::from_millis(10),
    );
    // Still the hub — the second tap did not stack another one.
    let (stack, _) = menu_parts(&state);
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Root));
}

#[test]
fn double_shift_respects_the_config_switch() {
    let mut state = make_test_state();
    state.config.keys.double_shift = Some(false);
    let t = Instant::now();
    handle_key(&mut state, shift_press(), t);
    handle_key(
        &mut state,
        shift_press(),
        t + std::time::Duration::from_millis(10),
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn double_shift_available_flag_drives_the_hint() {
    let mut state = make_test_state();
    let t = Instant::now();
    assert!(!state.double_shift.available());
    handle_key(&mut state, shift_press(), t);
    assert!(state.double_shift.available());
}

#[test]
fn question_mark_opens_help_over_the_hub() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, ctrl(','), now);
    handle_key(&mut state, key(KeyCode::Char('?')), now);
    let (stack, _) = menu_parts(&state);
    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
}

#[test]
fn question_mark_on_an_empty_prompt_opens_help_directly() {
    let mut state = make_test_state();
    let now = Instant::now();
    handle_key(&mut state, key(KeyCode::Char('?')), now);
    let (stack, _) = menu_parts(&state);
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
}

#[test]
fn question_mark_types_into_a_live_filter_instead_of_stacking_help() {
    let mut state = make_test_state();
    // Help itself is filter-first (§5.8) — `?` there is a printable char.
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = MenuUiState::default();
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('?')));
    assert_eq!(stack.depth(), 1);
    assert_eq!(ui.query, "?");
}

#[test]
fn ctrl_c_in_edit_mode_still_closes_the_whole_stack() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState {
        mode: MenuMode::Edit {
            buffer: "x".to_string(),
            cursor: 1,
        },
        ..MenuUiState::default()
    };
    handle_menu_key(&mut state, &mut stack, &mut ui, ctrl('c'));
    assert!(stack.is_empty());
}

#[test]
fn enter_on_a_hub_item_row_opens_the_bottom_picker() {
    let mut state = make_test_state();
    let mut stack = MenuStack::root(MenuId::Root);
    let mut ui = MenuUiState {
        cursor: RowId::new("model"),
        ..MenuUiState::default()
    };
    let actions = handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker { .. })
    ));
    assert_eq!(
        actions,
        vec![Action::Send(pacode_types::Request::ListModels)]
    );
}
