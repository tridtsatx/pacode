use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use pacode_config::Paths;
use pacode_render::Theme;
use pacode_types::Config;

use crate::keys_picker::handle_picker_key;
use crate::state::{AppState, Focus, Overlay, ThemePickerStep};

fn make_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn setup_state_with_picker() -> (AppState, TempDir) {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());
    let mut config = Config::default();
    config.theme.name = "pacode-dark".to_string();

    let mut state = AppState::new(config, "0.1.0-dev".into(), 80, 24);
    state.paths = paths;

    let original_theme = state.theme.clone();
    let original_name = state.config.theme.name.clone();
    let user_themes = pacode_config::user_theme_names(&state.paths);

    state.focus = Focus::Overlay(Overlay::ThemePicker {
        index: 0,
        original_theme: Box::new(original_theme),
        original_name,
        step: ThemePickerStep::SelectTheme,
        user_themes,
    });

    (state, tmp)
}

#[test]
fn test_theme_picker_move_previews() {
    let (mut state, _tmp) = setup_state_with_picker();
    let dark_theme = Theme::from_palette(
        &pacode_render::builtin_palettes()[0], // pacode-dark
        true,
    );
    let light_theme = Theme::from_palette(
        &pacode_render::builtin_palettes()[1], // pacode-light
        true,
    );

    assert_eq!(state.theme, dark_theme);

    // Press Down -> index becomes 1 (pacode-light)
    handle_picker_key(&mut state, make_key(KeyCode::Down));

    if let Focus::Overlay(Overlay::ThemePicker { index, .. }) = &state.focus {
        assert_eq!(*index, 1);
    } else {
        panic!("unexpected focus");
    }

    // Theme should now be light
    assert_eq!(state.theme, light_theme);
}

#[test]
fn test_theme_picker_esc_restores() {
    let (mut state, _tmp) = setup_state_with_picker();
    let dark_theme = Theme::from_palette(&pacode_render::builtin_palettes()[0], true);

    // Move to index 1 (pacode-light)
    handle_picker_key(&mut state, make_key(KeyCode::Down));
    assert_ne!(state.theme, dark_theme);

    // Press Esc -> should restore dark theme and Normal focus
    handle_picker_key(&mut state, make_key(KeyCode::Esc));

    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.theme, dark_theme);
    assert_eq!(state.config.theme.name, "pacode-dark");
}

#[test]
fn test_theme_picker_enter_normal_row_persists() {
    let (mut state, _tmp) = setup_state_with_picker();
    let light_theme = Theme::from_palette(&pacode_render::builtin_palettes()[1], true);

    // Move to index 1 (pacode-light)
    handle_picker_key(&mut state, make_key(KeyCode::Down));
    // Press Enter
    handle_picker_key(&mut state, make_key(KeyCode::Enter));

    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.config.theme.name, "pacode-light");
    assert_eq!(state.theme, light_theme);
}

#[test]
fn test_theme_picker_create_new_flow() {
    let (mut state, _tmp) = setup_state_with_picker();

    // 4 built-ins + 0 user themes + 1 create = 5 rows (indices 0..=4)
    // Move to index 4 ("Create new one…")
    for _ in 0..4 {
        handle_picker_key(&mut state, make_key(KeyCode::Down));
    }

    if let Focus::Overlay(Overlay::ThemePicker { index, step, .. }) = &state.focus {
        assert_eq!(*index, 4);
        assert_eq!(*step, ThemePickerStep::SelectTheme);
    } else {
        panic!("unexpected focus");
    }

    // Press Enter to open Step 2 (SelectBase)
    handle_picker_key(&mut state, make_key(KeyCode::Enter));

    if let Focus::Overlay(Overlay::ThemePicker { index, step, .. }) = &state.focus {
        assert_eq!(*index, 0);
        assert_eq!(*step, ThemePickerStep::SelectBase);
    } else {
        panic!("unexpected focus");
    }

    // Press Enter on base 0 (pacode-dark) -> creates custom.toml
    handle_picker_key(&mut state, make_key(KeyCode::Enter));

    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.config.theme.name, "custom");

    // Check custom.toml was written
    let theme_file = state
        .paths
        .config_file
        .parent()
        .unwrap()
        .join("themes/custom.toml");
    assert!(theme_file.exists());

    // Check toast was pushed
    assert!(!state.toasts.is_empty());
    let toast = &state.toasts[0];
    assert!(toast.title.contains("custom"));
}

#[test]
fn test_theme_picker_create_new_increments_filename() {
    let (mut state, _tmp) = setup_state_with_picker();

    // Seed custom.toml
    let themes_dir = state.paths.config_file.parent().unwrap().join("themes");
    std::fs::create_dir_all(&themes_dir).unwrap();
    std::fs::write(themes_dir.join("custom.toml"), "name = 'custom'\n").unwrap();

    // Open Create new one (row index 4)
    for _ in 0..4 {
        handle_picker_key(&mut state, make_key(KeyCode::Down));
    }
    // Enter Step 2
    handle_picker_key(&mut state, make_key(KeyCode::Enter));
    // Pick base -> creates custom-2.toml
    handle_picker_key(&mut state, make_key(KeyCode::Enter));

    assert_eq!(state.config.theme.name, "custom-2");
    assert!(themes_dir.join("custom-2.toml").exists());
}
