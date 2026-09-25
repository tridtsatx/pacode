use super::*;

#[test]
fn test_input_basic_editing() {
    let mut state = InputState::default();
    assert!(state.is_empty());
    assert_eq!(state.cursor, 0);

    state.insert_str("hello");
    assert_eq!(state.text, "hello");
    assert_eq!(state.cursor, 5);

    state.move_left();
    assert_eq!(state.cursor, 4);

    state.insert_char('!');
    assert_eq!(state.text, "hell!o");
    assert_eq!(state.cursor, 5);

    state.backspace();
    assert_eq!(state.text, "hello");
    assert_eq!(state.cursor, 4);

    state.delete();
    assert_eq!(state.text, "hell");
    assert_eq!(state.cursor, 4);

    state.home();
    assert_eq!(state.cursor, 0);

    state.end();
    assert_eq!(state.cursor, 4);
}

#[test]
fn test_delete_word() {
    let mut state = InputState::default();
    state.insert_str("foo bar baz  ");
    state.delete_word();
    assert_eq!(state.text, "foo bar ");
    state.delete_word();
    assert_eq!(state.text, "foo ");
    state.delete_word();
    assert_eq!(state.text, "");
}

#[test]
fn test_history_navigation() {
    let mut state = InputState::default();
    state.insert_str("first");
    let t1 = state.take();
    assert_eq!(t1, "first");
    assert!(state.is_empty());

    state.insert_str("second");
    let t2 = state.take();
    assert_eq!(t2, "second");

    state.insert_str("draft in progress");
    state.history_up();
    assert_eq!(state.text, "second");

    state.history_up();
    assert_eq!(state.text, "first");

    // Can't go further up
    state.history_up();
    assert_eq!(state.text, "first");

    state.history_down();
    assert_eq!(state.text, "second");

    state.history_down();
    assert_eq!(state.text, "draft in progress");
}

#[test]
fn test_wrapping_and_cursor() {
    let mut state = InputState::default();
    assert_eq!(state.wrapped_lines(80), 1);
    assert_eq!(state.cursor_position(80), (0, 2));

    state.insert_str("short");
    assert_eq!(state.wrapped_lines(80), 1);
    assert_eq!(state.cursor_position(80), (0, 7));

    // Multi-line via newlines
    state.insert_str("\nsecond\nthird");
    assert_eq!(state.wrapped_lines(80), 3);
    assert_eq!(state.cursor_position(80), (2, 7));
}

#[test]
fn test_trailing_space_cursor() {
    let mut state = InputState::default();
    state.insert_str("/model ");
    assert_eq!(state.cursor_position(80), (0, 2 + 7));
    assert_eq!(state.wrapped_lines(80), 1);
}

#[test]
fn test_wrap_word_boundary() {
    // Words wrap whole at the last boundary that fits; the separating space
    // still occupies cells (non-trimming contract).
    let lines = wrap_text_non_trimming("aaa bbb ccc", 7);
    assert_eq!(lines, vec!["aaa bbb", " ccc"]);
    assert_eq!(lines.concat(), "aaa bbb ccc");
}

#[test]
fn test_wrap_long_word_hard_break() {
    // A word longer than the line is split mid-word on grapheme boundaries.
    assert_eq!(
        wrap_text_non_trimming("averylongword", 5),
        vec!["avery", "longw", "ord"]
    );
    // The boundary wrap happens first, then the oversized word is chunked.
    assert_eq!(
        wrap_text_non_trimming("hi averylongword bye", 5),
        vec!["hi ", "avery", "longw", "ord ", "bye"]
    );
}

#[test]
fn test_wrap_cjk_no_spaces() {
    // No whitespace at all: CJK (2 cells each) falls back to grapheme splits.
    assert_eq!(
        wrap_text_non_trimming("你好世界测试", 4),
        vec!["你好", "世界", "测试"]
    );
}

#[test]
fn test_wrap_trailing_whitespace_preserved() {
    // Spaces are real characters: they fill the line and overflow forward.
    assert_eq!(wrap_text_non_trimming("abc  ", 4), vec!["abc ", " "]);
    assert_eq!(wrap_text_non_trimming("ab   cd", 4), vec!["ab  ", " cd"]);
}

#[test]
fn test_wrap_edge_cases() {
    assert_eq!(wrap_text_non_trimming("", 10), vec![""]);
    // width 0 is clamped to 1: hard break every grapheme.
    assert_eq!(wrap_text_non_trimming("ab", 0), vec!["a", "b"]);
    assert_eq!(wrap_text_non_trimming("ab", 1), vec!["a", "b"]);
    assert_eq!(wrap_text_non_trimming("a\nb\n", 10), vec!["a", "b", ""]);
    assert_eq!(wrap_text_non_trimming("ab\r\ncd", 10), vec!["ab", "cd"]);
}

#[test]
fn test_wrap_url_hard_break_no_char_loss() {
    let url = "see https://example.com/some/long/path ok";
    let lines = wrap_text_non_trimming(url, 20);
    // Nothing is dropped or reordered, and no line exceeds the width.
    assert_eq!(lines.concat(), url);
    assert!(lines.iter().all(|l| display_width(l) <= 20));
    // The URL is longer than a line, so it must have been split.
    assert!(lines.len() > 2);
}

#[test]
fn test_wrap_cursor_consistency() {
    let mut state = InputState::default();
    state.insert_str("foo barbaz");
    // Outer width 10 -> text width 8: "foo " stays, "barbaz" wraps to line 1.
    assert_eq!(state.wrap_lines(10), vec!["foo ", "barbaz"]);
    assert_eq!(state.cursor_position(10), (1, 8));

    // Cursor right after "foo" sits at the end of line 0.
    state.cursor = 3;
    assert_eq!(state.cursor_position(10), (0, 5));
    // After the separating space, still line 0 (the space trails the line).
    state.cursor = 4;
    assert_eq!(state.cursor_position(10), (0, 6));
}

#[test]
fn test_wrapped_position_of_byte() {
    // Single line: the column is the display width of the text before it.
    let mut state = InputState::default();
    state.insert_str("see [Image 2] ok");
    let byte = state.text.find("[Image 2]").unwrap();
    assert_eq!(state.wrapped_position_of_byte(byte, 80), (0, 4));

    // Outer width 10 → text width 8: "see " fills line 0, "[Image " wraps
    // to line 1 — the token's byte lands on the line where it STARTS.
    assert_eq!(state.wrapped_position_of_byte(byte, 10), (1, 0));

    // A token split mid-word: "a b c d e [Image 3]" at text width 6 wraps
    // into ["a b c ", "d e ", "[Image", " 3]"] — the space after `[Image`
    // overflows onto line 3 as leading whitespace (non-trimming wrap), so
    // '[' is line 2 col 0 and '3' is line 3 col 1.
    let mut state = InputState::default();
    state.insert_str("a b c d e [Image 3]");
    let byte = state.text.find("[Image 3]").unwrap();
    assert_eq!(state.wrapped_position_of_byte(byte, 8), (2, 0));
    let byte = state.text.find('3').unwrap();
    assert_eq!(state.wrapped_position_of_byte(byte, 8), (3, 1));

    // Wide chars before the token count display cells, not bytes: "你好 "
    // is 5 cells, so the token starts at col 0 of wrapped line 1.
    let mut state = InputState::default();
    state.insert_str("你好 [Image 1]");
    let byte = state.text.find("[Image 1]").unwrap();
    assert_eq!(state.wrapped_position_of_byte(byte, 10), (1, 0));

    // Multi-line text: the '\n' separator is bridged, the token maps to
    // line 1 at its column there.
    let mut state = InputState::default();
    state.insert_str("ab\ncd [Image 1]");
    let byte = state.text.find("[Image 1]").unwrap();
    assert_eq!(state.wrapped_position_of_byte(byte, 20), (1, 3));

    // A byte past the end lands on the last line's end.
    assert_eq!(
        state.wrapped_position_of_byte(state.text.len() + 100, 20),
        (1, 12)
    );
}

#[test]
fn scroll_reconciliation_depends_only_on_current_cursor_and_viewport() {
    let mut state = InputState::default();
    state.insert_str("0\n1\n2\n3\n4\n5");
    state.input_scroll = 99;

    state.reconcile_scroll(20, 2);
    assert_eq!(state.input_scroll, 4);

    state.cursor = 0;
    state.reconcile_scroll(20, 2);
    assert_eq!(state.input_scroll, 0);
}
