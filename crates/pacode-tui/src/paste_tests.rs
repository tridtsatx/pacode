use std::time::Instant;

use pacode_types::{Config, ToastLevel};
use tempfile::TempDir;

use super::*;
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
        query: String::new(),
        index: 0,
    });
    state.model_picker.query = "claude".to_string();
    state.input.text = "prompt text".to_string();

    handle_paste(&mut state, "-3-opus".to_string(), now);

    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker { .. })
    ));
    assert_eq!(state.model_picker.query, "claude-3-opus");

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

pub(crate) fn real_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        w,
        h,
        image::Rgb([20, 120, 220]),
    ));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode png");
    out
}

fn wl_paste_png_runner(bytes: Vec<u8>) -> MockCommandRunner {
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
        Ok(bytes),
    );
    runner
}

#[test]
fn test_image_paste_inserts_a_chip_and_keeps_the_prepared_image() {
    let mut state = make_state();
    let runner = wl_paste_png_runner(real_png(40, 20));

    let now = Instant::now();
    handle_paste_image_with_runner(&mut state, true, &runner, ClipboardPlatform::Linux, now);

    assert!(
        state.input.text.starts_with("[Image 1 \u{b7} "),
        "{}",
        state.input.text
    );
    assert!(state.input.text.ends_with("] "));
    assert_eq!(state.attachments.len(), 1);
    // No temp file and no `@path` any more.
    assert!(!state.input.text.contains('@'));

    let toast = state.toasts.back().unwrap();
    assert_eq!(toast.level, ToastLevel::Info);
    assert_eq!(toast.title, "Image attached");
}

#[test]
fn test_undecodable_clipboard_image_is_reported_not_inserted() {
    let mut state = make_state();
    let runner = wl_paste_png_runner(b"png_bytes".to_vec());

    handle_paste_image_with_runner(
        &mut state,
        true,
        &runner,
        ClipboardPlatform::Linux,
        Instant::now(),
    );

    assert!(state.input.text.is_empty());
    assert!(state.attachments.is_empty());
    let toast = state.toasts.back().unwrap();
    assert_eq!(toast.level, ToastLevel::Error);
}

#[test]
fn test_backspace_on_the_chip_removes_the_whole_chip() {
    let mut state = make_state();
    let runner = wl_paste_png_runner(real_png(8, 8));
    handle_paste_image_with_runner(
        &mut state,
        true,
        &runner,
        ClipboardPlatform::Linux,
        Instant::now(),
    );
    state.input.backspace(); // the space after the chip
    assert!(state.input.text.ends_with(']'));
    state.input.backspace(); // the chip, as a unit
    assert!(state.input.text.is_empty());
    assert_eq!(state.input.cursor, 0);
}

#[test]
fn test_empty_bracketed_paste_triggers_image_paste() {
    let mut state = make_state();
    let runner = wl_paste_png_runner(real_png(8, 8));

    let now = Instant::now();
    // Empty paste string delivered by terminal
    handle_paste_with_runner(
        &mut state,
        String::new(),
        &runner,
        ClipboardPlatform::Linux,
        now,
    );

    assert!(state.input.text.starts_with("[Image 1"));
}

#[test]
fn test_pasting_an_image_path_attaches_the_file() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("shot one.png");
    std::fs::write(&file, real_png(16, 16)).unwrap();
    let mut state = make_state();

    // Terminals quote or escape the path on drag-and-drop.
    let dropped = format!("'{}'", file.display());
    handle_paste(&mut state, dropped, Instant::now());

    assert!(
        state.input.text.starts_with("[Image 1"),
        "{}",
        state.input.text
    );
    assert_eq!(state.attachments.len(), 1);
}

#[test]
fn test_pasting_plain_text_that_is_not_an_image_path_stays_text() {
    let mut state = make_state();
    handle_paste(&mut state, "/no/such/file.png".to_string(), Instant::now());
    assert_eq!(state.input.text, "/no/such/file.png");
    assert!(state.attachments.is_empty());
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
    let mut state = make_state();

    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        paste_key("wl-paste", &["--list-types"]),
        Ok(b"image/png\n".to_vec()),
    );
    runner.responses.insert(
        paste_key("wl-paste", &["--type", "image/png", "--no-newline"]),
        Ok(real_png(8, 8)),
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
        state.input.text.starts_with("[Image 1"),
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
