//! §5.4: scope groups, the detail card (binding / default / scope / ⚠), the
//! capture flow — clean bind, conflict naming the owner, second-press
//! takeover, `esc` without a write — and `ctrl+r` factory reset.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::Config;

use super::*;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::menu::{MenuStack, StatusKind, resolve_cursor};

fn state() -> AppState {
    AppState::new(Config::default(), "0.1.0".into(), 120, 34)
}

/// A state whose config paths point at `dir` — write paths stay off the real
/// config dir.
fn state_at(dir: &std::path::Path) -> AppState {
    let mut s = state();
    s.paths = pacode_config::Paths::under(dir);
    s
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pacode-keys-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn find_row<'v>(view: &'v MenuView, key: &str) -> &'v MenuRow {
    view.rows
        .iter()
        .find(|r| r.id().is_some_and(|i| i.as_str() == key))
        .unwrap_or_else(|| panic!("row {key} not found"))
}

#[test]
fn scope_sections_cover_every_action_in_order() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);

    let sections: Vec<&str> = view
        .rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Section { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    // `menu` has no rebindable actions — its section is not drawn.
    assert_eq!(sections, ["[global]", "[prompt]", "[rail]", "[panel]"]);

    // Every action produces exactly one selectable row under its scope.
    let seen: Vec<String> = view
        .rows
        .iter()
        .filter_map(|r| r.id().map(|i| i.as_str().to_string()))
        .collect();
    assert_eq!(seen.len(), Keymap::action_names().len());
    for (name, action) in Keymap::action_names() {
        assert!(seen.iter().any(|id| id == name), "missing row {name}");
        // And the row sits under the right header: the section label of the
        // last Section row before it equals the action's scope.
        let pos = view
            .rows
            .iter()
            .position(|r| r.id().is_some_and(|i| i.as_str() == *name))
            .expect("row");
        let scope_label = view.rows[..pos]
            .iter()
            .rev()
            .find_map(|r| match r {
                MenuRow::Section { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .expect("section above");
        assert_eq!(
            scope_label,
            format!("[{}]", action.scope().label()),
            "{name} filed under {scope_label}"
        );
    }
}

#[test]
fn the_row_tail_lists_live_chords_and_the_override_star() {
    let mut s = state();
    s.keymap.set_binding(
        KeyAction::FollowAgent,
        Binding {
            code: KeyCode::F(7),
            mods: KeyModifiers::NONE,
        },
    );
    let ui = MenuUiState::default();
    let view = view(&s, &ui);

    let MenuRow::Item { right, .. } = find_row(&view, "next_agent") else {
        panic!("next_agent must be an Item");
    };
    assert_eq!(right.as_deref(), Some("alt+down, ctrl+j"));

    let MenuRow::Item { right, .. } = find_row(&view, "follow_agent") else {
        panic!("follow_agent must be an Item");
    };
    assert_eq!(right.as_deref(), Some("f7 *"));
}

#[test]
fn the_detail_card_names_binding_default_scope_and_description() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);
    let detail = find_row(&view, "prev_agent").detail().expect("detail");

    assert_eq!(detail.title, "prev_agent");
    let field = |name: &str| -> &str {
        detail
            .lines
            .iter()
            .find_map(|l| match l {
                DetailLine::Field { label, value } if label == name => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("field {name} missing: {:?}", detail.lines))
    };
    assert_eq!(field("binding"), "alt+up, ctrl+k");
    assert_eq!(field("default"), "alt+up, ctrl+k");
    assert_eq!(field("scope"), "rail");
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("previous agent")))
    );
}

#[test]
fn alt_arrows_carry_the_terminal_warning_with_the_fallback() {
    let s = state();
    let ui = MenuUiState::default();
    let v = view(&s, &ui);
    let detail = find_row(&v, "next_agent").detail().expect("detail");

    let warning = detail
        .lines
        .iter()
        .find_map(|l| match l {
            DetailLine::Text(t) if t.contains("alt+down") => Some(t.as_str()),
            _ => None,
        })
        .expect("a ⚠ line for alt+down");
    assert!(warning.starts_with('⚠'), "{warning}");
    assert!(warning.contains("ctrl+j still works"), "{warning}");

    // ASCII mode degrades the sign, not the message.
    let mut s = state();
    s.config.ui.ascii_only = true;
    let v = view(&s, &ui);
    let detail = find_row(&v, "next_agent").detail().expect("detail");
    assert!(
        detail.lines.iter().any(
            |l| matches!(l, DetailLine::Text(t) if t.starts_with("! ") && t.contains("alt+down"))
        ),
        "ascii fallback missing: {:?}",
        detail.lines
    );

    // A safe chord carries no warning.
    let detail = find_row(&v, "scroll_up").detail().expect("detail");
    assert!(
        !detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains('⚠'))),
        "scroll_up must not warn: {:?}",
        detail.lines
    );
}

#[test]
fn direct_entry_lands_a_single_keys_level() {
    let mut s = state();
    crate::keys_menu::open(&mut s, MenuId::Keys);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &s.focus else {
        panic!("expected the menu overlay, got {:?}", s.focus);
    };
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Keys));
}

#[test]
fn enter_on_an_action_row_starts_the_capture() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    assert!(matches!(
        ui.mode,
        MenuMode::Capture {
            action: "stop_agent",
            pending: None,
        }
    ));
}

#[test]
fn a_clean_chord_binds_at_once_and_persists() {
    let dir = tmpdir("clean");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::F(5)));

    assert!(matches!(ui.mode, MenuMode::List));
    assert_eq!(
        s.keymap.bindings_for(KeyAction::StopAgent),
        &[Binding {
            code: KeyCode::F(5),
            mods: KeyModifiers::NONE,
        }]
    );
    assert!(s.keymap.is_overridden(KeyAction::StopAgent));
    // The write landed in keybindings.toml — a reload reads it back.
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(
        loaded.keys.bindings.get("stop_agent").map(String::as_str),
        Some("f5")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conflicting_chord_names_the_owner_and_waits() {
    let dir = tmpdir("conflict");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('p'));

    // Still capturing — the conflict is parked, not committed.
    assert!(matches!(
        ui.mode,
        MenuMode::Capture {
            pending: Some(_),
            ..
        }
    ));
    let status = ui.status.as_ref().expect("conflict status");
    assert_eq!(status.kind, StatusKind::Warn);
    assert!(
        status.text.contains("session_picker"),
        "the owner is named: {}",
        status.text
    );
    assert!(status.text.contains("ctrl+p"), "{}", status.text);
    // Nothing was bound and nothing hit the disk.
    assert_eq!(
        s.keymap.bindings_for(KeyAction::StopAgent),
        KeyAction::StopAgent.default()
    );
    assert!(
        !s.paths.keybindings_file().exists(),
        "a parked conflict writes nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_second_identical_press_takes_the_chord() {
    let dir = tmpdir("steal");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('p'));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('p'));

    assert!(matches!(ui.mode, MenuMode::List));
    let taken = Binding {
        code: KeyCode::Char('p'),
        mods: KeyModifiers::CONTROL,
    };
    assert_eq!(s.keymap.bindings_for(KeyAction::StopAgent), &[taken]);
    // The owner lost the chord — in the keymap and on disk alike.
    assert!(s.keymap.bindings_for(KeyAction::SessionPicker).is_empty());
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(
        loaded.keys.bindings.get("stop_agent").map(String::as_str),
        Some("ctrl+p")
    );
    assert_eq!(
        loaded
            .keys
            .bindings
            .get("session_picker")
            .map(String::as_str),
        Some("")
    );
    // The unbind reads back: a fresh keymap has no chord on session_picker.
    let (keymap, warnings) = Keymap::from_config(&loaded.keys);
    assert!(warnings.is_empty());
    assert!(keymap.bindings_for(KeyAction::SessionPicker).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_takeover_keeps_the_owners_other_chords() {
    let dir = tmpdir("steal-partial");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("follow_agent"),
        ..MenuUiState::default()
    };

    // Steal `ctrl+k` from prev_agent — `alt+up` must survive on the owner.
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('k'));
    assert!(matches!(
        ui.mode,
        MenuMode::Capture {
            pending: Some(_),
            ..
        }
    ));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('k'));

    assert_eq!(
        s.keymap.bindings_for(KeyAction::PrevAgent),
        &[Binding {
            code: KeyCode::Up,
            mods: KeyModifiers::ALT,
        }]
    );
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(
        loaded.keys.bindings.get("prev_agent").map(String::as_str),
        Some("alt+up")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn esc_leaves_the_capture_without_a_write() {
    let dir = tmpdir("esc");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    // Park a conflict first — esc must retire both the mode and the status.
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('p'));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Esc));

    assert!(matches!(ui.mode, MenuMode::List));
    assert!(ui.status.is_none());
    assert!(!stack.is_empty(), "esc in capture leaves the menu open");
    assert_eq!(
        s.keymap.bindings_for(KeyAction::StopAgent),
        KeyAction::StopAgent.default()
    );
    assert!(!s.paths.keybindings_file().exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn captured_ctrl_p_goes_to_the_submenu_not_the_base() {
    // §5.4: in Capture every key but esc is the menu's — `ctrl+p` is cursor-up
    // for the base, but here it is just another chord (and a conflict).
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('p'));

    assert_eq!(stack.depth(), 1, "the menu is still open");
    assert!(matches!(ui.mode, MenuMode::Capture { .. }));
}

#[test]
fn an_unspellable_key_stays_in_the_capture() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    crate::keys_menu::handle_menu_key(
        &mut s,
        &mut stack,
        &mut ui,
        key(KeyCode::Modifier(
            crossterm::event::ModifierKeyCode::LeftShift,
        )),
    );
    assert!(matches!(ui.mode, MenuMode::Capture { .. }));
    assert_eq!(ui.status.as_ref().map(|s| s.kind), Some(StatusKind::Warn));
}

#[test]
fn rebinding_the_same_chord_is_no_conflict() {
    let dir = tmpdir("same");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    // `s` is already stop_agent's chord — it binds again without a warning.
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('s')));
    assert!(matches!(ui.mode, MenuMode::List));
    assert_eq!(
        s.keymap.bindings_for(KeyAction::StopAgent),
        &[Binding {
            code: KeyCode::Char('s'),
            mods: KeyModifiers::NONE,
        }]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_r_restores_the_factory_binding() {
    let dir = tmpdir("reset");
    let mut s = state_at(&dir);
    pacode_config::update_config_value(
        &s.paths,
        "keys.stop_agent",
        pacode_config::toml::Value::String("f5".to_string()),
    )
    .expect("seed override");
    s.keymap.set_binding(
        KeyAction::StopAgent,
        Binding {
            code: KeyCode::F(5),
            mods: KeyModifiers::NONE,
        },
    );

    let mut stack = MenuStack::root(MenuId::Keys);
    let mut ui = MenuUiState {
        cursor: RowId::new("stop_agent"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('r'));

    assert_eq!(
        s.keymap.bindings_for(KeyAction::StopAgent),
        KeyAction::StopAgent.default()
    );
    assert!(!s.keymap.is_overridden(KeyAction::StopAgent));
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(loaded.keys.bindings.get("stop_agent"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_filter_matches_name_and_description() {
    let s = state();
    let ui = MenuUiState::default();
    let mut v = view(&s, &ui);
    v.apply_filter("paste_clipboard");
    let kept: Vec<_> = v
        .rows
        .iter()
        .filter(|r| r.is_selectable())
        .filter_map(|r| r.id().map(|i| i.as_str()))
        .collect();
    assert_eq!(kept, ["paste_clipboard"]);

    let mut v = view(&s, &ui);
    v.apply_filter("clipboard");
    let kept: Vec<_> = v
        .rows
        .iter()
        .filter(|r| r.is_selectable())
        .filter_map(|r| r.id().map(|i| i.as_str()))
        .collect();
    assert!(kept.contains(&"paste_clipboard"), "{kept:?}");
}

#[test]
fn the_cursor_lands_on_the_first_action_not_a_header() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);
    let sel = resolve_cursor(&view.rows, &ui.cursor).expect("a selection");
    // The first `global`-scoped action in registry order.
    assert_eq!(
        view.rows[sel].id().map(RowId::as_str),
        Some("files_overlay")
    );
}
