//! §5.8: the palette index, the ranking rules and the per-kind `enter`
//! behaviour — plus the `?` regressions (plain char in a live filter and in
//! a non-empty prompt).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::Config;

use super::*;
use crate::keys::handle_key;
use crate::keys_menu::handle_menu_key;
use crate::state::{AppState, Focus};
use crate::ui::menu::{MenuLevel, MenuStack, resolve_cursor, selectable, view_for};

fn state() -> AppState {
    // The dynamic registry is process-global — tests share the binary, so
    // keep the index deterministic by clearing leftovers from other tests.
    crate::commands::clear_dynamic_commands();
    AppState::new(Config::default(), "0.1.0".into(), 120, 34)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ui_with_query(query: &str) -> MenuUiState {
    MenuUiState {
        query: query.to_string(),
        ..MenuUiState::default()
    }
}

/// The ids of the selectable rows the base would draw — provider output
/// after `view_for`'s own local-filter pass.
fn drawn_ids(state: &AppState, stack: &MenuStack, ui: &MenuUiState) -> Vec<RowId> {
    let view = view_for(stack, state, ui);
    selectable(&view.rows).map(|(_, id)| id).collect()
}

#[test]
fn the_index_covers_all_four_kinds() {
    let s = state();
    let view = view(&s, &MenuUiState::default());
    for prefix in ["cmd:", "menu:", "set:", "key:"] {
        assert!(
            view.rows
                .iter()
                .any(|r| r.id().is_some_and(|i| i.as_str().starts_with(prefix))),
            "missing {prefix} rows"
        );
    }
    // One known member per kind, plus the kind sections of the empty-query
    // browse layout.
    for id in [
        "cmd:quit",
        "menu:settings",
        "set:ui.color",
        "key:follow_agent",
    ] {
        assert!(
            view.rows.iter().any(|r| r.id() == Some(&RowId::new(id))),
            "missing row {id}"
        );
    }
    for section in ["Commands", "Menus", "Settings", "Keys"] {
        assert!(
            view.rows
                .iter()
                .any(|r| matches!(r, MenuRow::Section { label, .. } if label.as_str() == section)),
            "missing section {section}"
        );
    }
    // The hub and Help itself are not jump targets.
    assert!(!view.rows.iter().any(|r| {
        r.id()
            .is_some_and(|i| i.as_str() == "menu:root" || i.as_str() == "menu:help")
    }));
}

#[test]
fn plug_query_ranks_slash_plugins_first() {
    let s = state();
    let stack = MenuStack::root(MenuId::Help);
    let ui = ui_with_query("plug");
    let ids = drawn_ids(&s, &stack, &ui);
    assert_eq!(ids.first(), Some(&RowId::new("cmd:plugins")));
    // The Plugins submenu and `plugins.*` settings still make the list —
    // behind the command (kind order, §5.8).
    assert!(ids.contains(&RowId::new("menu:plugins")));
    assert!(ids.iter().any(|i| i.as_str() == "set:plugins.enabled"));
    let cmd = ids.iter().position(|i| i.as_str() == "cmd:plugins");
    let menu = ids.iter().position(|i| i.as_str() == "menu:plugins");
    assert!(cmd.is_some() && menu.is_some() && cmd < menu);
}

#[test]
fn an_exact_command_name_beats_a_substring() {
    // "mode" is the full name of /mode and a prefix of /model — the exact
    // hit leaves the tier list before any substring hit (§5.8).
    let s = state();
    let stack = MenuStack::root(MenuId::Help);
    let ui = ui_with_query("mode");
    let ids = drawn_ids(&s, &stack, &ui);
    assert_eq!(ids.first(), Some(&RowId::new("cmd:mode")));
    let mode = ids.iter().position(|i| i.as_str() == "cmd:mode");
    let model = ids.iter().position(|i| i.as_str() == "cmd:model");
    assert!(mode.is_some() && model.is_some() && mode < model);
}

#[test]
fn enter_on_a_set_row_lands_in_settings_with_the_filter() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = MenuUiState::default();
    on_row(&mut s, &mut stack, &mut ui, &RowId::new("set:ui.color"));
    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Settings));
    // The fresh level carries the dotted key as its filter and the cursor
    // already sits on the setting's row.
    assert_eq!(ui.query, "ui.color");
    assert_eq!(ui.cursor, RowId::new("ui.color"));
    let view = view_for(&stack, &s, &ui);
    let sel = resolve_cursor(&view.rows, &ui.cursor);
    assert_eq!(
        sel.and_then(|i| view.rows[i].id()),
        Some(&RowId::new("ui.color"))
    );
}

#[test]
fn enter_on_a_key_row_lands_in_keys_on_the_action() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = MenuUiState::default();
    on_row(&mut s, &mut stack, &mut ui, &RowId::new("key:follow_agent"));
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Keys));
    assert_eq!(ui.cursor, RowId::new("follow_agent"));
    let view = view_for(&stack, &s, &ui);
    let sel = resolve_cursor(&view.rows, &ui.cursor);
    assert_eq!(
        sel.and_then(|i| view.rows[i].id()),
        Some(&RowId::new("follow_agent"))
    );
}

#[test]
fn enter_on_a_menu_row_pushes_the_submenu() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = ui_with_query("sess");
    on_row(&mut s, &mut stack, &mut ui, &RowId::new("menu:sessions"));
    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Sessions));
    // The new level starts clean; Help's query was saved into its level.
    assert!(ui.query.is_empty());
    assert_eq!(stack.levels()[0].query, "sess");
}

#[test]
fn esc_after_a_palette_jump_returns_to_help() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = ui_with_query("sess");
    on_row(&mut s, &mut stack, &mut ui, &RowId::new("menu:sessions"));
    handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Esc));
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
    // Help comes back exactly as it was — query and all (§3.5).
    assert_eq!(ui.query, "sess");
}

#[test]
fn enter_on_a_cmd_row_executes_and_drains_the_stack() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    stack.push(MenuLevel::new(MenuId::Sessions));
    let mut ui = MenuUiState::default();
    let actions = on_row(&mut s, &mut stack, &mut ui, &RowId::new("cmd:quit"));
    assert_eq!(actions, vec![Action::Quit]);
    assert!(stack.is_empty());
}

#[test]
fn question_mark_appends_to_a_live_help_query() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Help);
    let mut ui = ui_with_query("ab");
    handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('?')));
    assert_eq!(ui.query, "ab?");
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Help));
}

#[test]
fn question_mark_in_a_nonempty_prompt_is_a_plain_char() {
    let mut s = state();
    s.input.text = "x".to_string();
    s.input.cursor = 1;
    handle_key(&mut s, key(KeyCode::Char('?')), Instant::now());
    assert_eq!(s.input.text, "x?");
    assert_eq!(s.focus, Focus::Normal);
}
