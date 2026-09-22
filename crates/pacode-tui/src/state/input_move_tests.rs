use super::*;

// -- word jumps (ctrl+left/right) --

#[test]
fn test_move_word_left_boundaries() {
    let mut state = InputState::default();
    state.insert_str("foo   bar baz");
    // chars: foo(0-2) spaces(3-5) bar(6-8) space(9) baz(10-12)
    assert_eq!(state.cursor, 13);
    state.move_word_left();
    assert_eq!(state.cursor, 10); // start of "baz"
    state.move_word_left();
    assert_eq!(state.cursor, 6); // start of "bar"
    state.move_word_left();
    assert_eq!(state.cursor, 0); // start of "foo"
    // Already at the start: no-op.
    state.move_word_left();
    assert_eq!(state.cursor, 0);
}

#[test]
fn test_move_word_right_boundaries() {
    let mut state = InputState::default();
    state.insert_str("foo   bar baz");
    state.cursor = 0;
    // From inside a word the boundary is the NEXT word's start — the
    // same spot delete_word_forward would delete up to.
    state.move_word_right();
    assert_eq!(state.cursor, 6); // start of "bar"
    state.move_word_right();
    assert_eq!(state.cursor, 10); // start of "baz"
    state.move_word_right();
    assert_eq!(state.cursor, 13); // end of text
    state.move_word_right();
    assert_eq!(state.cursor, 13); // no-op at the end
}

#[test]
fn test_move_word_right_from_whitespace() {
    let mut state = InputState::default();
    state.insert_str("foo   bar");
    state.cursor = 4; // inside the whitespace run
    // From whitespace the boundary is the next word's end.
    state.move_word_right();
    assert_eq!(state.cursor, 9); // end of "bar"
}

#[test]
fn test_move_word_unicode_cjk() {
    let mut state = InputState::default();
    state.insert_str("你好 world");
    // chars: 你(0) 好(1) ' '(2) world(3-7) — cursor is a CHAR index.
    state.cursor = 0;
    state.move_word_right();
    assert_eq!(state.cursor, 3); // start of "world"
    state.move_word_left();
    assert_eq!(state.cursor, 0); // start of "你好"
    state.cursor = 8;
    state.move_word_left();
    assert_eq!(state.cursor, 3);
}

#[test]
fn test_move_word_empty_and_whitespace_only() {
    let mut state = InputState::default();
    state.move_word_left(); // empty: no-op
    state.move_word_right();
    assert_eq!(state.cursor, 0);

    state.insert_str("   ");
    state.move_word_right();
    assert_eq!(state.cursor, 3); // whitespace to the end
    state.move_word_left();
    assert_eq!(state.cursor, 0);
}

// -- whole-input boundaries (ctrl+up/down) --

#[test]
fn test_move_to_start_end() {
    let mut state = InputState::default();
    state.insert_str("foo\nbar baz");
    state.move_to_start();
    assert_eq!(state.cursor, 0);
    state.move_to_end();
    assert_eq!(state.cursor, 11);
}

// -- logical line delete (alt+delete) --

#[test]
fn test_delete_line_middle() {
    let mut state = InputState::default();
    state.insert_str("a\nb\nc");
    state.cursor = 2; // on 'b'
    state.delete_line();
    assert_eq!(state.text, "a\nc");
    assert_eq!(state.cursor, 2); // where the deleted line started
}

#[test]
fn test_delete_line_first() {
    let mut state = InputState::default();
    state.insert_str("a\nb\nc");
    state.cursor = 0;
    state.delete_line();
    assert_eq!(state.text, "b\nc");
    assert_eq!(state.cursor, 0);
}

#[test]
fn test_delete_line_last() {
    let mut state = InputState::default();
    state.insert_str("a\nb\nc");
    state.cursor = 4; // on 'c'
    state.delete_line();
    assert_eq!(state.text, "a\nb");
    assert_eq!(state.cursor, 3); // clamped to where the line was
}

#[test]
fn test_delete_line_single() {
    let mut state = InputState::default();
    state.insert_str("abc");
    state.cursor = 1;
    state.delete_line();
    assert_eq!(state.text, "");
    assert_eq!(state.cursor, 0);
}

#[test]
fn test_delete_line_empty_and_trailing_newline() {
    let mut state = InputState::default();
    state.delete_line(); // empty: no-op
    assert_eq!(state.text, "");

    // Cursor on the empty last logical line removes the preceding '\n'.
    state.insert_str("a\n");
    state.delete_line();
    assert_eq!(state.text, "a");
    assert_eq!(state.cursor, 1);
}

#[test]
fn test_delete_line_cursor_on_newline() {
    let mut state = InputState::default();
    state.insert_str("a\nb");
    state.cursor = 1; // on the '\n' — counts as sitting on line "a"
    state.delete_line();
    assert_eq!(state.text, "b");
    assert_eq!(state.cursor, 0);
}

// -- vertical moves across wrapped visual lines (Up/Down) --

#[test]
fn test_move_vert_single_line_returns_false() {
    let mut state = InputState::default();
    assert!(!state.move_up(80)); // empty
    assert!(!state.move_down(80));

    state.insert_str("short");
    assert!(!state.move_up(80));
    assert!(!state.move_down(80));
    assert_eq!(state.cursor, 5);
}

#[test]
fn test_move_up_down_logical_lines() {
    let mut state = InputState::default();
    state.insert_str("l1\nl2\nl3");
    // chars: l1(0-1) \n(2) l2(3-4) \n(5) l3(6-7); cursor 8 = line 2, col 2.
    assert_eq!(state.cursor, 8);
    assert!(state.move_up(80));
    assert_eq!(state.cursor, 5); // end of "l2"
    assert!(state.move_up(80));
    assert_eq!(state.cursor, 2); // end of "l1"
    assert!(!state.move_up(80)); // first line
    assert_eq!(state.cursor, 2);

    assert!(state.move_down(80));
    assert_eq!(state.cursor, 5);
    assert!(state.move_down(80));
    assert_eq!(state.cursor, 8); // end of "l3"
    assert!(!state.move_down(80)); // last line
    assert_eq!(state.cursor, 8);
}

#[test]
fn test_move_up_down_wrapped_lines() {
    let mut state = InputState::default();
    state.insert_str("aaaa bbbb cccc");
    // Outer width 8 -> text width 6: wraps "aaaa ", "bbbb ", "cccc".
    assert_eq!(state.wrap_lines(8), vec!["aaaa ", "bbbb ", "cccc"]);
    assert_eq!(state.cursor, 14); // line 2, col 4
    assert!(state.move_up(8));
    assert_eq!(state.cursor, 9); // line 1 col 4 -> 5 + 4
    assert!(state.move_up(8));
    assert_eq!(state.cursor, 4); // line 0 col 4 -> the space after "aaaa"
    assert!(!state.move_up(8));
}

#[test]
fn test_move_vert_sticky_goal_column() {
    let mut state = InputState::default();
    state.insert_str("aaaaa\nbb\nccccc");
    // chars: aaaaa(0-4) \n(5) bb(6-7) \n(8) ccccc(9-13)
    assert_eq!(state.cursor, 14); // line 2, col 5
    assert!(state.move_up(80));
    assert_eq!(state.cursor, 8); // "bb" clamps goal 5 to its end
    assert!(state.move_up(80));
    assert_eq!(state.cursor, 5); // goal 5 restored on "aaaaa"
    assert!(state.move_down(80));
    assert_eq!(state.cursor, 8); // clamped again...
    assert!(state.move_down(80));
    assert_eq!(state.cursor, 14); // ...and restored on the wide line
}

#[test]
fn test_move_vert_goal_cleared_by_other_edits() {
    let mut state = InputState::default();
    state.insert_str("aaaaa\nbb\nccccc");
    state.move_up(80);
    assert_eq!(state.vert_goal, Some(5));
    state.move_left(); // any other cursor move clears it
    assert_eq!(state.vert_goal, None);
    state.move_up(80);
    state.insert_char('x'); // text edits clear it too
    assert_eq!(state.vert_goal, None);
}

#[test]
fn test_move_vert_cjk_width() {
    let mut state = InputState::default();
    state.insert_str("你好世界");
    // Outer width 6 -> text width 4: two 2-cell chars per line.
    assert_eq!(state.wrap_lines(6), vec!["你好", "世界"]);
    assert_eq!(state.cursor, 4); // line 1, col 4
    assert!(state.move_up(6));
    assert_eq!(state.cursor, 2); // goal 4 -> both 2-wide chars fit
    assert!(state.move_down(6));
    assert_eq!(state.cursor, 4);
}

#[test]
fn test_move_vert_clamps_to_grapheme_boundary() {
    let mut state = InputState::default();
    state.insert_str("abcde\n你好");
    state.cursor = 1; // "abcde" col 1
    assert!(state.move_down(80));
    // Goal col 1 lands before the 2-wide '你' — never mid-grapheme.
    assert_eq!(state.cursor, 6); // start of "你好"

    // Goal col 3 lands after '你' (w=2); '好' (w=2) would overshoot.
    state.vert_goal = Some(3);
    state.cursor = 1;
    assert!(state.move_down(80));
    assert_eq!(state.cursor, 7); // between 你 and 好
}

#[test]
fn test_move_vert_at_wrap_boundary() {
    let mut state = InputState::default();
    state.insert_str("aaa bbb");
    // Outer width 7 -> text width 5: wraps "aaa ", "bbb". A cursor right
    // after the trailing space renders at the end of line 0.
    assert_eq!(state.wrap_lines(7), vec!["aaa ", "bbb"]);
    state.cursor = 4;
    assert!(state.move_down(7));
    // Goal col 4 clamps to "bbb" width 3 -> end of input.
    assert_eq!(state.cursor, 7);
}
