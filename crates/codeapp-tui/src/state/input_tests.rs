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
