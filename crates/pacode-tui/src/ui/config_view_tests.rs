use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_types::Config;

use super::*;
use crate::state::AppState;
use crate::state::Overlay;

fn make_test_state() -> AppState {
    let config = Config::default();
    let mut state = AppState::new(config, "0.1.0".into(), 120, 34);
    state.config_view = ConfigViewState::new(&state.config);
    state.focus = Focus::Overlay(Overlay::ConfigPicker);
    state
}

#[test]
fn test_filtering_matches_on_label_key_and_description() {
    // 1. Match on label
    let by_label = filter_entries("Mouse Support");
    assert!(
        by_label.iter().any(|e| e.dotted_key == "ui.mouse"),
        "Should match ui.mouse by label"
    );

    // 2. Match on dotted key
    let by_key = filter_entries("exec.yield");
    assert!(
        by_key
            .iter()
            .any(|e| e.dotted_key == "exec.yield_after_secs"),
        "Should match exec.yield_after_secs by key"
    );

    // 3. Match on description
    let by_desc = filter_entries("clipboard");
    assert!(
        by_desc.iter().any(|e| e.dotted_key == "ui.auto_copy"),
        "Should match ui.auto_copy by description 'clipboard'"
    );
}

#[test]
fn test_navigation_skips_section_headings() {
    let mut state = make_test_state();
    let matching = filter_entries(&state.config_view.query);
    let rows = build_rows(&matching);

    // First row in rows is a section header, but selected selectable index 0 is an entry!
    assert!(
        matches!(rows[0], ConfigRow::Section(_)),
        "Row 0 must be section header"
    );
    assert_eq!(state.config_view.selected, 0);
    assert_eq!(
        matching[state.config_view.selected].dotted_key,
        "provider.default"
    );

    // Press Down -> moves selected to 1
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down);
    assert_eq!(state.config_view.selected, 1);
    assert_eq!(
        matching[state.config_view.selected].dotted_key,
        "provider.effort"
    );

    // Press Up -> moves selected back to 0
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_key(&mut state, up);
    assert_eq!(state.config_view.selected, 0);
    assert_eq!(
        matching[state.config_view.selected].dotted_key,
        "provider.default"
    );
}

#[test]
fn test_bool_toggle_writes_dotted_key() {
    let mut state = make_test_state();

    // Set query to "ui.mouse" so it's the selected item (index 0)
    state.config_view.query = "ui.mouse".to_string();
    state.config_view.selected = 0;
    assert!(state.config.ui.mouse, "Default mouse should be true");

    // Press Enter to toggle
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter);

    // In-memory config is toggled to false
    assert!(!state.config.ui.mouse, "ui.mouse should now be false");

    // Toggle again
    handle_key(&mut state, enter);
    assert!(state.config.ui.mouse, "ui.mouse should now be true");
}

#[test]
fn test_ctrl_r_removes_override() {
    let mut state = make_test_state();

    // Set query to "exec.yield_after_secs"
    state.config_view.query = "exec.yield_after_secs".to_string();
    state.config_view.selected = 0;

    // Mutate the config away from default (10 -> 99)
    state.config.exec.yield_after_secs = 99;
    assert_eq!(state.config.exec.yield_after_secs, 99);

    // Ctrl+R resets to default
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    handle_key(&mut state, ctrl_r);

    assert_eq!(state.config.exec.yield_after_secs, 10);
}

#[test]
fn test_scroll_offset_keeps_selection_visible_in_short_viewport() {
    let matching = filter_entries("");
    let rows = build_rows(&matching);
    let viewport_height = 5;

    // Initially at top
    let mut scroll = 0;
    scroll = adjust_scroll(0, &rows, viewport_height, scroll);
    assert_eq!(scroll, 0);

    // Find a selectable index that is further down the rows (e.g. index 10)
    let sel_idx = 10;
    let target_row = rows
        .iter()
        .position(|r| match r {
            ConfigRow::Setting {
                selectable_index, ..
            } => *selectable_index == sel_idx,
            ConfigRow::Section(_) => false,
        })
        .unwrap();

    scroll = adjust_scroll(sel_idx, &rows, viewport_height, scroll);

    // Assert that target_row is within [scroll, scroll + viewport_height)
    assert!(
        target_row >= scroll && target_row < scroll + viewport_height,
        "Target row {target_row} must be within visible window [{scroll}, {})",
        scroll + viewport_height
    );
}

#[test]
fn test_validation_error_keeps_edit_box_open() {
    let mut state = make_test_state();

    // Filter to "exec.yield_after_secs"
    state.config_view.query = "exec.yield_after_secs".to_string();
    state.config_view.selected = 0;

    // Enter opens inline editing
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut state, enter);
    assert!(state.config_view.editing.is_some());

    // Enter an invalid integer (out of range: max is 3600, let's type "99999")
    if let Some(ref mut edit) = state.config_view.editing {
        edit.buffer = "99999".to_string();
        edit.cursor = 5;
    }

    // Press Enter to confirm edit
    handle_key(&mut state, enter);

    // Validation fails -> error is set and editing box stays open!
    assert!(state.config_view.error.is_some());
    assert!(state.config_view.editing.is_some());

    // Esc cancels editing and clears error
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut state, esc);
    assert!(state.config_view.editing.is_none());
    assert!(state.config_view.error.is_none());
}
