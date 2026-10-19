//! Behaviour of the overlay model: what a key press asks the host to do.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use pacode_types::Config;

use super::*;

/// Rows a 40-row terminal shows, so page keys move by a page.
const PAGE: usize = 10;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(code)
    }
}

fn press(state: &mut SettingsState, config: &Config, code: KeyCode) -> SettingsOutcome {
    on_key(state, key(code), config, PAGE)
}

fn type_str(state: &mut SettingsState, config: &Config, text: &str) {
    for c in text.chars() {
        press(state, config, KeyCode::Char(c));
    }
}

/// Move the highlight onto `dotted_key` by tabbing to its section and walking
/// down, so a test reads as "the user is on this setting".
fn focus(state: &mut SettingsState, config: &Config, dotted_key: &str) {
    let section = registry::find_entry(dotted_key).map_or("", |e| e.section);
    while state.section() != section_index(&section_order(), section) {
        press(state, config, KeyCode::Right);
    }
    let target = section_entries(section)
        .position(|e| e.dotted_key == dotted_key)
        .unwrap_or_default();
    for _ in 0..target {
        press(state, config, KeyCode::Down);
    }
    assert_eq!(
        state.row(state.cursor()).map(|e| e.dotted_key),
        Some(dotted_key),
        "the highlight never reached {dotted_key}"
    );
}

/// Open the inline editor on the focused setting with an empty buffer.
fn edit_empty(state: &mut SettingsState, config: &Config) {
    press(state, config, KeyCode::Enter);
    assert!(state.editing().is_some(), "enter should open the editor");
    on_key(state, ctrl(KeyCode::Char('u')), config, PAGE);
    assert_eq!(state.editing().map(|(b, _)| b), Some(""));
}

#[test]
fn sections_follow_the_registry_order_and_cover_every_setting() {
    let order = section_order();
    let declared: Vec<&str> = registry::SECTIONS
        .iter()
        .copied()
        .filter(|s| order.contains(s))
        .collect();
    assert_eq!(
        &order[..declared.len()],
        &declared[..],
        "declared order is kept"
    );
    for entry in registry::SETTINGS {
        assert!(
            order.contains(&entry.section),
            "{} has no tab",
            entry.section
        );
        assert!(
            section_entries(entry.section).any(|e| e.dotted_key == entry.dotted_key),
            "{} is in no section list",
            entry.dotted_key
        );
    }
}

#[test]
fn a_section_the_order_list_forgot_is_still_reachable() {
    let order = section_order();
    // `section_index` is what a jump lands on, so an unknown name must not
    // panic and must not resolve past the end of the list.
    assert_eq!(section_index(&order, "not-a-section"), 0);
    assert_eq!(section_entries("not-a-section").count(), 0);
    assert_eq!(
        section_index(&order, "web"),
        order.iter().position(|s| *s == "web").unwrap()
    );
}

#[test]
fn esc_clears_the_search_before_it_closes_the_overlay() {
    let config = Config::default();
    let mut state = SettingsState::new();
    type_str(&mut state, &config, "shimmer");
    assert_eq!(state.query(), "shimmer");
    assert!(state.row_count() > 0);

    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Consumed
    );
    assert_eq!(state.query(), "");
    // Back to the section's own rows rather than the hits.
    assert_eq!(
        state.row_count(),
        section_entries(section_order()[state.section()]).count()
    );

    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Close
    );
}

#[test]
fn search_spans_sections_and_lands_on_the_first_match() {
    let config = Config::default();
    let mut state = SettingsState::new();
    type_str(&mut state, &config, "model");
    let hits = search_matches("model");
    assert!(
        hits.iter().any(|e| e.section == "ui") && hits.iter().any(|e| e.section != "ui"),
        "expected matches in more than one section"
    );
    assert_eq!(state.cursor(), 0);
    assert_eq!(state.row(0).map(|e| e.dotted_key), Some(hits[0].dotted_key));
    assert_eq!(
        state.section(),
        section_index(&section_order(), hits[0].section)
    );

    // Backspace narrows the filter instead of leaving anything behind.
    press(&mut state, &config, KeyCode::Backspace);
    assert_eq!(state.query(), "mode");
    assert_eq!(
        state.row(0).map(|e| e.dotted_key),
        Some(search_matches("mode")[0].dotted_key)
    );
}

#[test]
fn a_search_that_matches_nothing_leaves_no_rows_and_stays_open() {
    let config = Config::default();
    let mut state = SettingsState::new();
    type_str(&mut state, &config, "zzz");
    assert_eq!(state.row_count(), 0);
    assert!(state.row(0).is_none());
    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Consumed
    );
    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Close
    );
}

#[test]
fn enum_cycles_and_wraps_past_the_last_option() {
    let mut config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "ui.animation");
    let mut seen = vec![registry::read_value(&config, "ui.animation").unwrap_or_default()];
    for _ in 0..3 {
        match press(&mut state, &config, KeyCode::Enter) {
            SettingsOutcome::Commit { key, value } => {
                assert_eq!(key, "ui.animation");
                let next = value.as_str().unwrap_or_default().to_string();
                registry::apply_to_config(&mut config, key, &value);
                seen.push(next);
            }
            other => panic!("expected a commit, got {other:?}"),
        }
    }
    assert_eq!(seen, ["smooth", "balanced", "eco", "smooth"]);
}

#[test]
fn space_changes_the_highlighted_setting_but_joins_a_search() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "ui.vim");
    match press(&mut state, &config, KeyCode::Char(' ')) {
        SettingsOutcome::Commit { key, value } => {
            assert_eq!(key, "ui.vim");
            assert_eq!(value, toml::Value::Boolean(true));
        }
        other => panic!("expected a commit, got {other:?}"),
    }
    // Once a query is running, space is part of it — a filter that could not
    // hold a space would be a filter that could not hold a phrase.
    type_str(&mut state, &config, "a b");
    assert_eq!(state.query(), "a b");
}

#[test]
fn an_out_of_range_value_says_why_and_keeps_the_editor_open() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "provider.max_retries");
    edit_empty(&mut state, &config);
    type_str(&mut state, &config, "999");

    assert_eq!(
        press(&mut state, &config, KeyCode::Enter),
        SettingsOutcome::Consumed
    );
    let err = state.error().unwrap_or_default().to_string();
    assert!(err.contains("out of range"), "{err}");
    assert!(
        err.contains("100"),
        "the bound belongs in the message: {err}"
    );
    // Still editing with the refused text intact, and the config untouched.
    assert_eq!(state.editing().map(|(b, _)| b), Some("999"));
    assert_eq!(
        registry::read_value(&config, "provider.max_retries").as_deref(),
        Some("5")
    );

    // The reason belongs to the value, not to the line: moving drops it.
    press(&mut state, &config, KeyCode::Esc);
    press(&mut state, &config, KeyCode::Down);
    assert!(state.error().is_none());
}

#[test]
fn an_unparseable_value_is_refused_rather_than_written() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "ui.ups");
    edit_empty(&mut state, &config);
    type_str(&mut state, &config, "fast");

    assert_eq!(
        press(&mut state, &config, KeyCode::Enter),
        SettingsOutcome::Consumed
    );
    let err = state.error().unwrap_or_default().to_string();
    assert!(err.contains("fast"), "{err}");
    assert!(
        err.contains("240"),
        "the accepted values belong in it: {err}"
    );
    assert_eq!(
        registry::read_value(&config, "ui.ups").as_deref(),
        Some("10")
    );
}

#[test]
fn a_valid_number_commits_through_the_registry() {
    let mut config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "provider.max_retries");
    edit_empty(&mut state, &config);
    type_str(&mut state, &config, "7");

    match press(&mut state, &config, KeyCode::Enter) {
        SettingsOutcome::Commit { key, value } => {
            assert_eq!(key, "provider.max_retries");
            registry::apply_to_config(&mut config, key, &value);
        }
        other => panic!("expected a commit, got {other:?}"),
    }
    assert!(state.editing().is_none());
    assert!(state.error().is_none());
    assert_eq!(
        registry::read_value(&config, "provider.max_retries").as_deref(),
        Some("7")
    );
}

#[test]
fn an_action_row_runs_its_command_and_refuses_a_reset() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "mcp.servers");
    assert_eq!(
        press(&mut state, &config, KeyCode::Enter),
        SettingsOutcome::Run { command: "/mcp" }
    );
    // Resetting would delete the server table, so ctrl+r refuses the row.
    assert_eq!(
        on_key(&mut state, ctrl(KeyCode::Char('r')), &config, PAGE),
        SettingsOutcome::Consumed
    );
}

#[test]
fn ctrl_r_resets_a_value_row() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "ui.vim");
    assert_eq!(
        on_key(&mut state, ctrl(KeyCode::Char('r')), &config, PAGE),
        SettingsOutcome::Reset { key: "ui.vim" }
    );
}

#[test]
fn tabs_wrap_around_the_section_list() {
    let config = Config::default();
    let mut state = SettingsState::new();
    let count = section_order().len();
    assert_eq!(
        press(&mut state, &config, KeyCode::Tab),
        SettingsOutcome::Consumed
    );
    assert_eq!(state.section(), 1);
    for _ in 0..(count - 1) {
        press(&mut state, &config, KeyCode::Tab);
    }
    assert_eq!(state.section(), 0);
    press(&mut state, &config, KeyCode::Left);
    assert_eq!(state.section(), count - 1);
    press(&mut state, &config, KeyCode::BackTab);
    assert_eq!(state.section(), count - 2);
}

#[test]
fn the_window_keeps_the_highlight_visible_at_any_height() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "ui.transcript_cells");
    let total = state.row_count();
    assert!(total > PAGE, "the ui section should overflow a short body");
    for height in 1..=total {
        let window = state.window(total, height);
        assert_eq!(window.len(), height.min(total), "height {height}");
        assert!(
            window.contains(&state.cursor()),
            "the highlight left the window at height {height}"
        );
    }
    // A body with no rows is nothing to draw, not a panic.
    assert_eq!(state.window(total, 0), 0..0);
    assert_eq!(state.window(0, 5), 0..0);
}

#[test]
fn the_editor_owns_the_keyboard_while_it_is_open() {
    let config = Config::default();
    let mut state = SettingsState::new();
    focus(&mut state, &config, "session.title_model");
    press(&mut state, &config, KeyCode::Enter);
    // Letters reach the buffer, not the filter.
    type_str(&mut state, &config, "haiku");
    assert_eq!(state.query(), "");
    assert_eq!(state.editing().map(|(b, _)| b), Some("haiku"));
    // Esc abandons the edit without closing or filtering.
    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Consumed
    );
    assert!(state.editing().is_none());
    assert_eq!(state.query(), "");
    assert_eq!(
        press(&mut state, &config, KeyCode::Esc),
        SettingsOutcome::Close
    );
}
