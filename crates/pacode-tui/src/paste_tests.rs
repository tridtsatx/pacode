use std::time::Instant;

use pacode_types::{Config, ToastLevel};
use tempfile::TempDir;

use super::*;
use crate::clipboard_read::PastedImages;
use crate::clipboard_read::clipboard_read_tests::MockCommandRunner;

fn make_state() -> AppState {
    let config = Config::default();
    AppState::new(config, "1.0.0".to_string(), 80, 24)
}

#[test]
fn test_multiline_paste_inserts_literal_newlines_and_does_not_submit() {
    let mut state = make_state();
    let now = Instant::now();

    let multiline_text = "line 1\nline 2\nline 3".to_string();
    let actions = handle_paste(&mut state, multiline_text, now);

    // Pasting must return no submit actions
    assert!(actions.is_empty());
    assert!(!state.turn_active);
    assert_eq!(state.input.text, "line 1\nline 2\nline 3");
    assert_eq!(state.input.cursor, 20);
}

#[test]
fn test_paste_crlf_normalized() {
    let mut state = make_state();
    let now = Instant::now();

    let text_crlf = "alpha\r\nbeta\rgamma".to_string();
    handle_paste(&mut state, text_crlf, now);

    assert_eq!(state.input.text, "alpha\nbeta\ngamma");
}

#[test]
fn test_paste_at_cursor_not_at_end() {
    let mut state = make_state();
    let now = Instant::now();

    state.input.text = "hello world".to_string();
    state.input.cursor = 5; // right after "hello"

    handle_paste(&mut state, " beautiful".to_string(), now);

    assert_eq!(state.input.text, "hello beautiful world");
    assert_eq!(state.input.cursor, 15);
}

#[test]
fn test_paste_size_cap_trips_and_reports() {
    let mut state = make_state();
    let now = Instant::now();

    // Create string exceeding PASTE_MAX_CHARS
    let oversized = "a".repeat(PASTE_MAX_CHARS + 100);
    handle_paste(&mut state, oversized, now);

    // Prompt contains exactly PASTE_MAX_CHARS
    assert_eq!(state.input.text.chars().count(), PASTE_MAX_CHARS);

    // Toast was pushed reporting truncation
    assert_eq!(state.toasts.len(), 1);
    let toast = &state.toasts[0];
    assert_eq!(toast.level, ToastLevel::Warn);
    assert!(toast.title.contains(&PASTE_MAX_CHARS.to_string()));
    assert!(
        toast
            .detail
            .as_ref()
            .unwrap()
            .contains(&(PASTE_MAX_CHARS + 100).to_string())
    );
}

#[test]
fn test_paste_with_model_picker_focused_goes_into_filter() {
    let mut state = make_state();
    let now = Instant::now();

    state.focus = Focus::Overlay(Overlay::ModelPicker {
        query: "claude".to_string(),
        index: 3,
    });
    state.input.text = "prompt text".to_string();

    handle_paste(&mut state, "-3-opus".to_string(), now);

    match &state.focus {
        Focus::Overlay(Overlay::ModelPicker { query, index }) => {
            assert_eq!(query, "claude-3-opus");
            assert_eq!(*index, 0); // index reset to top match
        }
        _ => panic!("focus changed unexpectedly"),
    }

    // Hidden prompt was NOT mutated
    assert_eq!(state.input.text, "prompt text");
}

#[test]
fn test_paste_with_session_picker_focused_goes_into_filter() {
    let mut state = make_state();
    let now = Instant::now();

    state.focus = Focus::Overlay(Overlay::SessionPicker {
        query: "sess".to_string(),
        index: 2,
    });
    state.input.text = "untouched".to_string();

    handle_paste(&mut state, "ion-1\nnewline".to_string(), now);

    match &state.focus {
        Focus::Overlay(Overlay::SessionPicker { query, index }) => {
            // multi-line text flattened into single-line filter
            assert_eq!(query, "session-1 newline");
            assert_eq!(*index, 0);
        }
        _ => panic!("focus changed unexpectedly"),
    }

    assert_eq!(state.input.text, "untouched");
}

#[test]
fn test_paste_with_config_picker_focused_goes_into_query() {
    let mut state = make_state();
    let now = Instant::now();

    state.focus = Focus::Overlay(Overlay::ConfigPicker);
    state.config_view.query = "the".to_string();
    state.input.text = "untouched".to_string();

    handle_paste(&mut state, "me".to_string(), now);

    assert_eq!(state.config_view.query, "theme");
    assert_eq!(state.input.text, "untouched");
}

#[test]
fn test_paste_with_overlay_without_filter_is_ignored() {
    let mut state = make_state();
    let now = Instant::now();

    state.focus = Focus::Overlay(Overlay::EffortPicker { index: 1 });
    state.input.text = "before paste".to_string();

    handle_paste(&mut state, "ignored text".to_string(), now);

    // Text did not leak into hidden prompt
    assert_eq!(state.input.text, "before paste");
}

#[test]
fn test_image_paste_inserts_at_path_token_pointing_to_existing_file() {
    let mut state = make_state();
    let tmp = TempDir::new().unwrap();
    state.pasted_images = PastedImages::new_in(tmp.path().to_path_buf());

    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    let fake_png = b"\x89PNG\r\n\x1a\nfake_image".to_vec();
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Ok(fake_png),
    );

    let now = Instant::now();
    handle_paste_image_with_runner(&mut state, true, &runner, ClipboardPlatform::Linux, now);

    // Prompt contains @<path>
    assert!(state.input.text.starts_with('@'));
    assert!(state.input.text.ends_with(".png "));

    let token_path_str = state.input.text.trim_start_matches('@').trim();
    let token_path = std::path::PathBuf::from(token_path_str);

    // File exists on disk
    assert!(token_path.exists());

    // User-facing toast explains metadata v1 limitation
    assert!(!state.toasts.is_empty());
    let toast = state.toasts.back().unwrap();
    assert_eq!(toast.level, ToastLevel::Info);
    assert_eq!(toast.title, "Image pasted");
    assert!(toast.detail.as_ref().unwrap().contains("metadata"));
    assert!(toast.detail.as_ref().unwrap().contains("pixels"));
}

#[test]
fn test_temp_file_removed_when_removed_from_prompt() {
    let mut state = make_state();
    let tmp = TempDir::new().unwrap();
    state.pasted_images = PastedImages::new_in(tmp.path().to_path_buf());

    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Ok(b"png_bytes".to_vec()),
    );

    let now = Instant::now();
    handle_paste_image_with_runner(&mut state, true, &runner, ClipboardPlatform::Linux, now);

    let token_path_str = state.input.text.trim_start_matches('@').trim();
    let token_path = std::path::PathBuf::from(token_path_str);
    assert!(token_path.exists());

    // User removes token from prompt
    state.input.text.clear();
    cleanup_unreferenced_images(&mut state);

    // File is removed from disk immediately
    assert!(!token_path.exists());
}

#[test]
fn test_empty_bracketed_paste_triggers_image_paste() {
    let mut state = make_state();
    let tmp = TempDir::new().unwrap();
    state.pasted_images = PastedImages::new_in(tmp.path().to_path_buf());

    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Ok(b"png_bytes".to_vec()),
    );

    let now = Instant::now();
    // Empty paste string delivered by terminal
    handle_paste_with_runner(
        &mut state,
        String::new(),
        &runner,
        ClipboardPlatform::Linux,
        now,
    );

    // Image clipboard was checked and inserted @path
    assert!(state.input.text.starts_with('@'));
}

#[test]
fn test_vim_mode_paste_normal_insert_visual() {
    let mut state = make_state();
    state.config.ui.vim = true;
    let now = Instant::now();

    // 1. Insert mode paste
    state.vim.mode = VimMode::Insert;
    state.input.text = "abc".to_string();
    state.input.cursor = 1;
    handle_paste(&mut state, "XYZ".to_string(), now);
    assert_eq!(state.input.text, "aXYZbc");
    assert_eq!(state.input.cursor, 4);

    // Undo in vim restores previous state
    let prev = state.vim.undo_stack.pop_back().unwrap();
    state.input.text = prev.0;
    state.input.cursor = prev.1;
    assert_eq!(state.input.text, "abc");
    assert_eq!(state.input.cursor, 1);

    // 2. Normal mode paste
    state.vim.mode = VimMode::Normal;
    state.input.text = "hello".to_string();
    state.input.cursor = 1; // 'e'
    handle_paste(&mut state, "!".to_string(), now);
    assert_eq!(state.input.text, "h!ello");
    assert_eq!(state.vim.mode, VimMode::Normal);

    // 3. Visual mode paste replaces selection
    state.vim.mode = VimMode::Visual;
    state.input.text = "hello world".to_string();
    state.vim.visual_anchor = Some(0);
    state.input.cursor = 4; // "hello" selected
    handle_paste(&mut state, "bye".to_string(), now);
    assert_eq!(state.input.text, "bye world");
    assert_eq!(state.vim.mode, VimMode::Normal);
    assert_eq!(state.vim.visual_anchor, None);
}

fn paste_key(binary: &str, args: &[&str]) -> (String, Vec<String>) {
    (
        binary.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
    )
}

#[test]
fn test_ctrl_v_pastes_clipboard_text_when_there_is_no_image() {
    let mut state = make_state();
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        paste_key("wl-paste", &["--no-newline", "--type", "text/plain"]),
        Ok(b"from clipboard".to_vec()),
    );

    let actions = handle_paste_clipboard_with_runner(
        &mut state,
        &runner,
        ClipboardPlatform::Linux,
        Instant::now(),
    );

    assert!(actions.is_empty());
    assert_eq!(state.input.text, "from clipboard");
}

#[test]
fn test_ctrl_v_prefers_an_image_over_text() {
    let tmp = TempDir::new().expect("tempdir");
    let mut state = make_state();
    state.pasted_images = PastedImages::new_in(tmp.path().to_path_buf());

    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        paste_key("wl-paste", &["--list-types"]),
        Ok(b"image/png\n".to_vec()),
    );
    runner.responses.insert(
        paste_key("wl-paste", &["--type", "image/png", "--no-newline"]),
        Ok(vec![0x89, 0x50, 0x4E, 0x47]),
    );
    runner.responses.insert(
        paste_key("wl-paste", &["--no-newline", "--type", "text/plain"]),
        Ok(b"text too".to_vec()),
    );

    handle_paste_clipboard_with_runner(
        &mut state,
        &runner,
        ClipboardPlatform::Linux,
        Instant::now(),
    );

    assert!(
        state.input.text.starts_with('@'),
        "got {}",
        state.input.text
    );
    assert!(!state.input.text.contains("text too"));
}

#[test]
fn test_ctrl_v_on_an_empty_clipboard_reports_and_inserts_nothing() {
    let mut state = make_state();
    let runner = MockCommandRunner::new();

    let actions = handle_paste_clipboard_with_runner(
        &mut state,
        &runner,
        ClipboardPlatform::Linux,
        Instant::now(),
    );

    assert!(actions.is_empty());
    assert_eq!(state.input.text, "");
    assert_eq!(state.toasts.len(), 1);
    assert_eq!(state.toasts[0].level, ToastLevel::Info);
}
