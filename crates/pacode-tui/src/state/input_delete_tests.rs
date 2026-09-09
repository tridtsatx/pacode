use super::*;

#[test]
fn test_delete_word_forward_at_end_of_line_is_noop() {
    let mut state = InputState::default();
    state.insert_str("hello world");
    assert_eq!(state.cursor, 11);
    state.delete_word_forward();
    assert_eq!(state.text, "hello world");
    assert_eq!(state.cursor, 11);
}

#[test]
fn test_delete_word_forward_removes_one_word_plus_spaces_after() {
    let mut state = InputState::default();
    state.insert_str("foo   bar baz");
    state.cursor = 0;
    state.delete_word_forward();
    assert_eq!(state.text, "bar baz");
    assert_eq!(state.cursor, 0);

    state.delete_word_forward();
    assert_eq!(state.text, "baz");
    assert_eq!(state.cursor, 0);

    state.delete_word_forward();
    assert_eq!(state.text, "");
    assert_eq!(state.cursor, 0);
}

#[test]
fn test_delete_word_forward_and_backward_symmetry() {
    // Starting with "first   second   third"
    // Backward delete starting from end removes "third", then "second   ", then "first   ".
    // Forward delete starting from start removes "first   ", then "second   ", then "third".
    let original = "first   second   third";

    // Forward deletion sequence
    let mut forward_state = InputState::default();
    forward_state.insert_str(original);
    forward_state.cursor = 0;

    forward_state.delete_word_forward();
    assert_eq!(forward_state.text, "second   third");

    forward_state.delete_word_forward();
    assert_eq!(forward_state.text, "third");

    forward_state.delete_word_forward();
    assert_eq!(forward_state.text, "");

    // Backward deletion sequence
    let mut backward_state = InputState::default();
    backward_state.insert_str(original);
    assert_eq!(backward_state.cursor, original.chars().count());

    backward_state.delete_word();
    assert_eq!(backward_state.text, "first   second   ");

    backward_state.delete_word();
    assert_eq!(backward_state.text, "first   ");

    backward_state.delete_word();
    assert_eq!(backward_state.text, "");

    // Exact word boundary deletion symmetry:
    // In "hello   world", cursor between the two tokens (at index 8, before 'w'):
    // Backward delete removes "hello   ", leaving "world".
    // Forward delete removes "world", leaving "hello   ".
    let mut s1 = InputState::default();
    s1.insert_str("hello   world");
    s1.cursor = 8;
    s1.delete_word();
    assert_eq!(s1.text, "world");

    let mut s2 = InputState::default();
    s2.insert_str("hello   world");
    s2.cursor = 8;
    s2.delete_word_forward();
    assert_eq!(s2.text, "hello   ");
}
