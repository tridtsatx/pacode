use super::*;
use pacode_types::TranscriptKind;

#[test]
fn test_is_bash_mode_and_command_text() {
    assert!(!is_bash_mode(""));
    assert!(!is_bash_mode("hello"));
    assert!(is_bash_mode("!"));
    assert!(is_bash_mode("!ls"));
    assert!(is_bash_mode("!echo hi"));

    assert_eq!(command_text("!ls -la"), "ls -la");
    assert_eq!(command_text("!"), "");
}

#[test]
fn test_bash_mode_entered_and_left_by_typing_and_deleting_bang() {
    let mut input = crate::state::InputState::default();
    assert!(!is_bash_mode(&input.text));

    // Typing '!' as first character enters bash mode
    input.insert_char('!');
    assert!(is_bash_mode(&input.text));

    input.insert_str("cargo check");
    assert!(is_bash_mode(&input.text));
    assert_eq!(command_text(&input.text), "cargo check");

    // Deleting back to empty leaves bash mode
    while !input.text.is_empty() {
        input.backspace();
    }
    assert!(!is_bash_mode(&input.text));
}

#[test]
fn test_is_interactive_rules() {
    // Known programs
    assert!(is_interactive("vim main.rs"));
    assert!(is_interactive("/usr/bin/nvim test.txt"));
    assert!(is_interactive("top"));
    assert!(is_interactive("htop"));
    assert!(is_interactive("less log.txt"));

    // User-prefixed with !
    assert!(is_interactive("!custom_interactive_tool --foo"));

    // Regular non-interactive
    assert!(!is_interactive("echo hello"));
    assert!(!is_interactive("cargo build"));
    assert!(!is_interactive("git status"));
}

#[test]
fn test_output_truncation_keeps_tail_and_notes() {
    let small_data = b"small output\n";
    let (out, trunc) = truncate_output(small_data, 100);
    assert_eq!(out, "small output\n");
    assert!(!trunc);

    let large_data = vec![b'x'; 200];
    let (out, trunc) = truncate_output(&large_data, 50);
    assert!(trunc);
    assert!(out.starts_with("[output truncated: kept last 50 bytes]\n"));
    assert!(out.ends_with(&"x".repeat(50)));
}

#[test]
fn test_non_zero_exit_is_visible() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let kind = TranscriptKind::BashCommand {
        command: "false".to_string(),
        output: "failure\n".to_string(),
        exit_code: Some(1),
        truncated: false,
    };
    let lines = crate::ui::dialog::render_item(&kind, None, 80, &opts, 0);
    let full_rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(full_rendered.contains("(exit 1)"));
    assert!(full_rendered.contains("failure"));
}

#[test]
fn test_enter_in_bash_mode_runs_instead_of_sending() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut state =
        crate::state::AppState::new(pacode_types::Config::default(), "0.1.0".to_string(), 80, 24);
    state.input.text = "!cargo test".to_string();
    state.input.cursor = 11;

    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let actions = crate::keys::handle_key(&mut state, key, std::time::Instant::now());

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        crate::keys::Action::RunBashCaptured { command } => {
            assert_eq!(command, "cargo test");
        }
        other => panic!("expected Action::RunBashCaptured, got {other:?}"),
    }
}
