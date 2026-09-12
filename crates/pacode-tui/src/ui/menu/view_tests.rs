use crossterm::event::{KeyCode, KeyModifiers};

use super::*;
use crate::ui::menu::Detail;

fn key(code: KeyCode, mods: KeyModifiers) -> KeyBinding {
    Binding { code, mods }
}

fn plain(code: KeyCode) -> KeyBinding {
    key(code, KeyModifiers::NONE)
}

#[test]
fn base_keys_cover_the_spec_table() {
    for b in [
        plain(KeyCode::Up),
        plain(KeyCode::Down),
        plain(KeyCode::Left),
        plain(KeyCode::Right),
        key(KeyCode::Char('p'), KeyModifiers::CONTROL),
        key(KeyCode::Char('n'), KeyModifiers::CONTROL),
        plain(KeyCode::PageUp),
        plain(KeyCode::PageDown),
        plain(KeyCode::Home),
        plain(KeyCode::End),
        plain(KeyCode::Tab),
        plain(KeyCode::BackTab),
        key(KeyCode::Tab, KeyModifiers::SHIFT),
        plain(KeyCode::Enter),
        plain(KeyCode::Esc),
        key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        plain(KeyCode::Char('?')),
        key(KeyCode::Char('?'), KeyModifiers::SHIFT),
    ] {
        assert!(is_base_key(&b), "{b:?} must be a base key");
    }
}

#[test]
fn submenu_actions_may_use_letters_space_and_ctrl() {
    for b in [
        plain(KeyCode::Char('i')),
        plain(KeyCode::Char('x')),
        plain(KeyCode::Char('r')),
        plain(KeyCode::Char(' ')),
        key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
    ] {
        assert!(!is_base_key(&b), "{b:?} must stay usable by submenus");
    }
}

#[test]
fn conflicts_are_reported_for_actions_and_chips() {
    let detail = Detail {
        title: String::new(),
        lines: Vec::new(),
        actions: vec![MenuAction::new(plain(KeyCode::Esc), "close")],
        search: Vec::new(),
    };
    let rows = vec![MenuRow::Check {
        id: RowId::from("c"),
        label: "c".to_string(),
        checked: false,
        warn: None,
        detail,
    }];
    let view = MenuView {
        rows,
        actions: vec![
            MenuAction::new(plain(KeyCode::Char('x')), "remove"),
            MenuAction::new(plain(KeyCode::Enter), "apply"),
        ],
        ..MenuView::new(MenuId::Import)
    };
    let conflicts = view.base_key_conflicts();
    assert_eq!(conflicts.len(), 2, "{conflicts:?}");
    assert!(conflicts.iter().any(|c| c.contains("apply")));
    assert!(conflicts.iter().any(|c| c.contains("close")));
}

#[test]
#[should_panic(expected = "base keys")]
fn with_actions_asserts_on_a_base_key_in_debug() {
    let _ = MenuView::new(MenuId::Plugins)
        .with_actions(vec![MenuAction::new(plain(KeyCode::Esc), "boom")]);
}

#[test]
fn with_actions_keeps_clean_actions() {
    let view = MenuView::new(MenuId::Plugins).with_actions(vec![
        MenuAction::new(plain(KeyCode::Char('i')), "install"),
        MenuAction::new(plain(KeyCode::Char('x')), "uninstall").enabled(false),
    ]);
    assert_eq!(view.actions.len(), 2);
    assert!(view.base_key_conflicts().is_empty());
}

#[test]
fn menu_id_gives_a_name_and_an_identifier_for_every_variant() {
    for id in MenuId::ALL {
        assert!(!id.title().is_empty());
        assert!(!id.slug().is_empty());
        assert!(id.slug().chars().all(|c| c.is_ascii_lowercase()));
    }
    // Distinct names and focus keys — the enum replaces the two match tables.
    let mut slugs: Vec<&str> = MenuId::ALL.iter().map(|i| i.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), MenuId::ALL.len());
    let mut keys: Vec<u8> = MenuId::ALL.iter().map(|i| i.focus_key()).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), MenuId::ALL.len());
    // Above the legacy `Focus`/`Overlay` focus-key range (0..=24).
    assert!(MenuId::ALL.iter().all(|i| i.focus_key() > 24));
}

#[test]
fn status_kinds_and_empty_state_construct() {
    assert_eq!(StatusLine::loading().kind, StatusKind::Loading);
    assert_eq!(StatusLine::offline_copy().kind, StatusKind::Warn);
    assert_eq!(StatusLine::error("nope").kind, StatusKind::Error);
    let empty = EmptyState::new("No plugins", "tab Discover");
    assert_eq!(empty.primary, "No plugins");
    assert_eq!(empty.hint, "tab Discover");
}
