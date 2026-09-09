use super::*;

#[test]
fn test_editor_resolution_precedence() {
    // Both VISUAL and EDITOR set -> VISUAL wins
    let (prog, args) = resolve_editor_from(|var| match var {
        "VISUAL" => Some("code --wait".to_string()),
        "EDITOR" => Some("nano".to_string()),
        _ => None,
    });
    assert_eq!(prog, "code");
    assert_eq!(args, vec!["--wait"]);

    // Only EDITOR set -> EDITOR wins
    let (prog, args) = resolve_editor_from(|var| match var {
        "VISUAL" => None,
        "EDITOR" => Some("nano".to_string()),
        _ => None,
    });
    assert_eq!(prog, "nano");
    assert!(args.is_empty());

    // Neither set -> falls back to vi
    let (prog, args) = resolve_editor_from(|_| None);
    assert_eq!(prog, "vi");
    assert!(args.is_empty());

    // Empty or whitespace VISUAL -> falls back to EDITOR
    let (prog, args) = resolve_editor_from(|var| match var {
        "VISUAL" => Some("   ".to_string()),
        "EDITOR" => Some("nvim -u NONE".to_string()),
        _ => None,
    });
    assert_eq!(prog, "nvim");
    assert_eq!(args, vec!["-u", "NONE"]);

    // Both empty -> falls back to vi
    let (prog, args) = resolve_editor_from(|var| match var {
        "VISUAL" => Some("".to_string()),
        "EDITOR" => Some("  ".to_string()),
        _ => None,
    });
    assert_eq!(prog, "vi");
    assert!(args.is_empty());
}

#[test]
fn test_argument_splitting() {
    let (prog, args) = resolve_editor_from(|_| Some("emacs  -nw  --quick".to_string()));
    assert_eq!(prog, "emacs");
    assert_eq!(args, vec!["-nw", "--quick"]);
}

#[test]
fn test_trim_single_trailing_newline() {
    assert_eq!(trim_single_trailing_newline(""), "");
    assert_eq!(trim_single_trailing_newline("hello"), "hello");
    assert_eq!(trim_single_trailing_newline("hello\n"), "hello");
    assert_eq!(trim_single_trailing_newline("hello\r\n"), "hello");
    assert_eq!(trim_single_trailing_newline("hello\n\n"), "hello\n");
    assert_eq!(trim_single_trailing_newline("hello\r\n\r\n"), "hello\r\n");
    assert_eq!(trim_single_trailing_newline("\n"), "");
    assert_eq!(trim_single_trailing_newline("\n\n"), "\n");
    assert_eq!(
        trim_single_trailing_newline("line1\n\nline2\n"),
        "line1\n\nline2"
    );
    assert_eq!(
        trim_single_trailing_newline("line1\n\nline2"),
        "line1\n\nline2"
    );
}

#[test]
fn test_temp_file_round_trip_fake_editor() {
    let args = vec![
        "-c".to_string(),
        "printf 'edited content\\n' > \"$1\"".to_string(),
        "--".to_string(),
    ];
    let result = edit_text_with("sh", &args, "initial prompt text");
    assert_eq!(result.expect("edit_text_with succeeds"), "edited content");
}

#[test]
fn test_fake_editor_modifies_initial_text() {
    let args = vec![
        "-c".to_string(),
        "printf '%s appended\\n' \"$(cat \"$1\")\" > \"$1\"".to_string(),
        "--".to_string(),
    ];
    let result = edit_text_with("sh", &args, "original");
    assert_eq!(
        result.expect("edit_text_with succeeds"),
        "original appended"
    );
}

#[test]
fn test_fake_editor_preserves_interior_newlines() {
    let args = vec![
        "-c".to_string(),
        "printf 'first\\n\\nsecond\\n' > \"$1\"".to_string(),
        "--".to_string(),
    ];
    let result = edit_text_with("sh", &args, "");
    assert_eq!(result.expect("edit_text_with succeeds"), "first\n\nsecond");
}

#[test]
fn test_fake_editor_non_zero_exit() {
    let args = vec!["-c".to_string(), "exit 42".to_string(), "--".to_string()];
    let result = edit_text_with("sh", &args, "initial");
    match result {
        Err(EditorError::NonZeroExit { cmd, status }) => {
            assert_eq!(cmd, "sh");
            assert_eq!(status.code(), Some(42));
        }
        other => panic!("expected NonZeroExit, got {other:?}"),
    }
}

#[test]
fn test_fake_editor_spawn_failed() {
    let result = edit_text_with("non_existent_binary_xyz_987", &[], "test");
    match result {
        Err(EditorError::Spawn { cmd, .. }) => {
            assert_eq!(cmd, "non_existent_binary_xyz_987");
        }
        other => panic!("expected Spawn error, got {other:?}"),
    }
}
