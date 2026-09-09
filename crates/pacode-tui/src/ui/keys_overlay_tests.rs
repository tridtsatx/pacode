use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use pacode_config::Paths;
use pacode_render::RenderOptions;
use pacode_types::{Config, ToastLevel};

use crate::binding::{Action as KeyAction, Binding, Keymap};
use crate::keys_picker::handle_picker_key;
use crate::state::{AppState, Focus, Overlay};

fn setup_state_with_keys_picker() -> (AppState, TempDir) {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());
    let mut config = Config::default();
    config.theme.name = "pacode-dark".to_string();

    let mut state = AppState::new(config, "0.1.0-dev".into(), 80, 24);
    state.paths = paths;
    state.focus = Focus::Overlay(Overlay::KeysPicker {
        index: 0,
        capturing: false,
    });

    (state, tmp)
}

#[test]
fn test_keys_overlay_draw_normal() {
    let (state, _tmp) = setup_state_with_keys_picker();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);

    terminal
        .draw(|f| {
            super::draw(f, f.area(), 0, false, &state, &opts);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .join("");

    assert!(text.contains("Keyboard Shortcuts"));
    assert!(text.contains("Cycle permission mode"));
    assert!(text.contains("shift+tab"));
    assert!(text.contains("select"));
    assert!(text.contains("enter rebind"));
    assert!(text.contains("ctrl+r reset"));
    assert!(text.contains("esc close"));
}

#[test]
fn test_keys_overlay_draw_capturing() {
    let (state, _tmp) = setup_state_with_keys_picker();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);

    terminal
        .draw(|f| {
            super::draw(f, f.area(), 0, true, &state, &opts);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .join("");

    assert!(text.contains("press a key…"));
    assert!(text.contains("press key to bind"));
    assert!(text.contains("esc cancel"));
}

#[test]
fn test_keys_overlay_draw_overridden() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;
    state.keymap.set_binding(
        action,
        Binding {
            code: KeyCode::Char('o'),
            mods: KeyModifiers::CONTROL,
        },
    );
    assert!(state.keymap.is_overridden(action));

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);

    terminal
        .draw(|f| {
            super::draw(f, f.area(), 0, false, &state, &opts);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .join("");

    assert!(text.contains("ctrl+o *"));
}

#[test]
fn test_navigation_clamping() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let count = Keymap::action_names().len();

    // At index 0, Up should clamp at 0
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );

    // 'k' also clamps at 0
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );

    // Down advances to 1
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 1,
            capturing: false
        })
    );

    // 'j' advances to 2
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 2,
            capturing: false
        })
    );

    // Advance to end
    for _ in 2..count {
        handle_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
    }
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: count - 1,
            capturing: false
        })
    );

    // Down at end clamps
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: count - 1,
            capturing: false
        })
    );
}

#[test]
fn test_enter_toggles_capturing() {
    let (mut state, _tmp) = setup_state_with_keys_picker();

    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: true
        })
    );
}

#[test]
fn test_esc_during_capture_cancels() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;
    let original_bindings = state.keymap.bindings_for(action).to_vec();

    // Enter capture mode
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: true
        })
    );

    // Press Esc during capture -> cancels capture, does not change keymap or close overlay
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
    );
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );
    assert_eq!(state.keymap.bindings_for(action), &original_bindings);
}

#[test]
fn test_esc_when_not_capturing_closes_overlay() {
    let (mut state, _tmp) = setup_state_with_keys_picker();

    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
    );
    assert_eq!(state.focus, Focus::Normal);
}

#[test]
fn test_captured_key_updates_keymap_and_persists() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;

    // Enter capture mode
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    // Capture ctrl+g (not bound to anything by default)
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
    );

    // Capturing is false
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );

    // Keymap is updated and marked overridden
    let bindings = state.keymap.bindings_for(action);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].code, KeyCode::Char('g'));
    assert_eq!(bindings[0].mods, KeyModifiers::CONTROL);
    assert!(state.keymap.is_overridden(action));

    // Config file carries the override
    let cfg = pacode_config::load(&state.paths).unwrap();
    assert_eq!(
        cfg.keys.bindings.get(action.name()).map(|s| s.as_str()),
        Some("ctrl+g")
    );
}

#[test]
fn test_conflicting_capture_pushes_warning_and_leaves_untouched() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;
    let original_bindings = state.keymap.bindings_for(action).to_vec();

    // Enter capture mode
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    // Press alt+b (which is default for FilesOverlay)
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
    );

    // Capture mode is left
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );

    // Keymap is untouched
    assert_eq!(state.keymap.bindings_for(action), &original_bindings);
    assert!(!state.keymap.is_overridden(action));

    // Toast pushed naming the conflicting action description
    assert!(!state.toasts.is_empty());
    let toast = state.toasts.back().unwrap();
    assert_eq!(toast.level, ToastLevel::Warn);
    assert!(toast.title.contains("conflict") || toast.title.contains("Conflict"));
    assert_eq!(
        toast.detail.as_deref(),
        Some(KeyAction::FilesOverlay.description())
    );
}

#[test]
fn test_ctrl_r_restores_default_and_removes_from_config() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;
    let default_bindings = state.keymap.bindings_for(action).to_vec();

    // First override it
    state.keymap.set_binding(
        action,
        Binding {
            code: KeyCode::Char('y'),
            mods: KeyModifiers::CONTROL,
        },
    );
    let act_name = action.name();
    let _ = pacode_config::update_config_value(
        &state.paths,
        &format!("keys.{act_name}"),
        pacode_config::toml::Value::String("ctrl+y".to_string()),
    );
    assert!(state.keymap.is_overridden(action));

    // Press ctrl+r
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    );

    // Default restored and is_overridden cleared
    assert_eq!(state.keymap.bindings_for(action), &default_bindings);
    assert!(!state.keymap.is_overridden(action));

    // Config file no longer carries the override
    let cfg = pacode_config::load(&state.paths).unwrap();
    assert_eq!(cfg.keys.bindings.get(action.name()), None);
}

#[test]
fn test_unrepresentable_key_rejected() {
    let (mut state, _tmp) = setup_state_with_keys_picker();
    let action = Keymap::action_names()[0].1;
    let original_bindings = state.keymap.bindings_for(action).to_vec();

    // Enter capture mode
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    // Send KeyCode::Null (which format_binding renders as "unknown")
    handle_picker_key(
        &mut state,
        KeyEvent::new(KeyCode::Null, KeyModifiers::empty()),
    );

    // Capture mode left
    assert_eq!(
        state.focus,
        Focus::Overlay(Overlay::KeysPicker {
            index: 0,
            capturing: false
        })
    );

    // Keymap untouched
    assert_eq!(state.keymap.bindings_for(action), &original_bindings);

    // Warning toast pushed
    assert!(!state.toasts.is_empty());
    let toast = state.toasts.back().unwrap();
    assert_eq!(toast.level, ToastLevel::Warn);
}
