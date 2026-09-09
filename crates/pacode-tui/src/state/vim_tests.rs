use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::state::input::InputState;

fn char_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn feed_str(vim: &mut VimState, input: &mut InputState, s: &str) {
    for c in s.chars() {
        handle(vim, input, char_key(c));
    }
}

#[test]
fn mode_transitions_and_insert_passthrough() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "hello".to_string(),
        cursor: 1,
        ..Default::default()
    };

    // Initial mode is Normal
    assert_eq!(vim.mode, VimMode::Normal);

    // 'i' enters Insert at cursor
    let eff = handle(&mut vim, &mut input, char_key('i'));
    assert_eq!(eff, VimEffect::Consumed);
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.cursor, 1);

    // In Insert mode, keys pass through
    let eff = handle(&mut vim, &mut input, char_key('x'));
    assert_eq!(eff, VimEffect::PassThrough);
    let eff = handle(&mut vim, &mut input, key(KeyCode::Enter));
    assert_eq!(eff, VimEffect::PassThrough);
    let eff = handle(&mut vim, &mut input, key(KeyCode::Backspace));
    assert_eq!(eff, VimEffect::PassThrough);

    // Esc returns to Normal
    let eff = handle(&mut vim, &mut input, key(KeyCode::Esc));
    assert_eq!(eff, VimEffect::Consumed);
    assert_eq!(vim.mode, VimMode::Normal);

    // 'a' enters Insert at cursor + 1
    input.cursor = 1;
    handle(&mut vim, &mut input, char_key('a'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.cursor, 2);
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // 'I' enters Insert at first non-blank
    input.text = "   foo".to_string();
    input.cursor = 5;
    handle(&mut vim, &mut input, char_key('I'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.cursor, 3);
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // 'A' enters Insert at line end
    input.text = "foo\nbar".to_string();
    input.cursor = 0;
    handle(&mut vim, &mut input, char_key('A'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.cursor, 3);
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // 'o' opens line below
    input.text = "foo\nbar".to_string();
    input.cursor = 0;
    handle(&mut vim, &mut input, char_key('o'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.text, "foo\n\nbar");
    assert_eq!(input.cursor, 4);
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // 'O' opens line above
    input.text = "foo\nbar".to_string();
    input.cursor = 4;
    handle(&mut vim, &mut input, char_key('O'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.text, "foo\n\nbar");
    assert_eq!(input.cursor, 4);
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // 'v' enters Visual, Esc leaves
    handle(&mut vim, &mut input, char_key('v'));
    assert_eq!(vim.mode, VimMode::Visual);
    assert_eq!(vim.visual_anchor, Some(4));
    handle(&mut vim, &mut input, key(KeyCode::Esc));
    assert_eq!(vim.mode, VimMode::Normal);
    assert_eq!(vim.visual_anchor, None);

    // 'v' toggle
    handle(&mut vim, &mut input, char_key('v'));
    assert_eq!(vim.mode, VimMode::Visual);
    handle(&mut vim, &mut input, char_key('v'));
    assert_eq!(vim.mode, VimMode::Normal);

    // Enter in Normal mode returns Submit
    let eff = handle(&mut vim, &mut input, key(KeyCode::Enter));
    assert_eq!(eff, VimEffect::Submit);
}

#[test]
fn motions_horizontal_and_vertical() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "line1\nline2\nline3".to_string(),
        cursor: 0,
        ..Default::default()
    };

    // 'l' single and counted
    handle(&mut vim, &mut input, char_key('l'));
    assert_eq!(input.cursor, 1);
    feed_str(&mut vim, &mut input, "3l");
    assert_eq!(input.cursor, 4);

    // 'h' single and counted
    handle(&mut vim, &mut input, char_key('h'));
    assert_eq!(input.cursor, 3);
    feed_str(&mut vim, &mut input, "2h");
    assert_eq!(input.cursor, 1);
    feed_str(&mut vim, &mut input, "5h");
    assert_eq!(input.cursor, 0); // clamps to line start

    // 'j' single and counted
    handle(&mut vim, &mut input, char_key('j'));
    assert_eq!(input.cursor, 6); // line2 start
    feed_str(&mut vim, &mut input, "1j");
    assert_eq!(input.cursor, 12); // line3 start

    // 'k' single and counted
    handle(&mut vim, &mut input, char_key('k'));
    assert_eq!(input.cursor, 6); // line2
    feed_str(&mut vim, &mut input, "1k");
    assert_eq!(input.cursor, 0); // line1
}

#[test]
fn motions_words_and_lines() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "foo bar, baz. qux".to_string(),
        cursor: 0,
        ..Default::default()
    };

    // 'w' single and counted
    handle(&mut vim, &mut input, char_key('w'));
    assert_eq!(input.cursor, 4); // 'bar, baz. qux'
    feed_str(&mut vim, &mut input, "2w");
    assert_eq!(input.cursor, 9); // 'baz. qux'

    // 'b' single and counted
    handle(&mut vim, &mut input, char_key('b'));
    assert_eq!(input.cursor, 7); // ','
    feed_str(&mut vim, &mut input, "2b");
    assert_eq!(input.cursor, 0);

    // 'e' single and counted
    handle(&mut vim, &mut input, char_key('e'));
    assert_eq!(input.cursor, 2); // 'o'
    feed_str(&mut vim, &mut input, "2e");
    assert_eq!(input.cursor, 7); // ','

    // 'W', 'B', 'E' (big words)
    input.cursor = 0;
    handle(&mut vim, &mut input, char_key('W'));
    assert_eq!(input.cursor, 4); // 'bar,'
    handle(&mut vim, &mut input, char_key('W'));
    assert_eq!(input.cursor, 9); // 'baz.'
    handle(&mut vim, &mut input, char_key('B'));
    assert_eq!(input.cursor, 4);
    handle(&mut vim, &mut input, char_key('E'));
    assert_eq!(input.cursor, 7); // ',' in 'bar,'

    // 0, ^, $
    input.text = "   hello world".to_string();
    input.cursor = 8;
    handle(&mut vim, &mut input, char_key('0'));
    assert_eq!(input.cursor, 0);
    handle(&mut vim, &mut input, char_key('^'));
    assert_eq!(input.cursor, 3);
    handle(&mut vim, &mut input, char_key('$'));
    assert_eq!(input.cursor, 13); // 'd'

    // gg, G
    input.text = "first\nsecond\nthird".to_string();
    input.cursor = 0;
    handle(&mut vim, &mut input, char_key('G'));
    assert_eq!(input.cursor, 13); // 'third'
    feed_str(&mut vim, &mut input, "gg");
    assert_eq!(input.cursor, 0);
    feed_str(&mut vim, &mut input, "2G");
    assert_eq!(input.cursor, 6); // 'second'
}

#[test]
fn motions_find_and_repeat() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "banana split".to_string(),
        cursor: 0,
        ..Default::default()
    };

    // 'f' find forward
    feed_str(&mut vim, &mut input, "fa");
    assert_eq!(input.cursor, 1);
    assert_eq!(vim.last_find, Some(('f', 'a')));

    // ';' repeat
    handle(&mut vim, &mut input, char_key(';'));
    assert_eq!(input.cursor, 3);
    handle(&mut vim, &mut input, char_key(';'));
    assert_eq!(input.cursor, 5);

    // ',' reverse repeat
    handle(&mut vim, &mut input, char_key(','));
    assert_eq!(input.cursor, 3);

    // 't' till forward
    feed_str(&mut vim, &mut input, "tn");
    assert_eq!(input.cursor, 3); // before 'n' at 4

    // 'F' find backward
    feed_str(&mut vim, &mut input, "Fb");
    assert_eq!(input.cursor, 0);
}

#[test]
fn operators_delete_and_change() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "hello world".to_string(),
        ..Default::default()
    };

    // dw
    feed_str(&mut vim, &mut input, "dw");
    assert_eq!(input.text, "world");
    assert_eq!(input.cursor, 0);
    assert_eq!(vim.registers, "hello ");

    // d$
    input.text = "hello world".to_string();
    input.cursor = 6;
    feed_str(&mut vim, &mut input, "d$");
    assert_eq!(input.text, "hello ");
    assert_eq!(input.cursor, 5);
    assert_eq!(vim.registers, "world");

    // dd single line
    input.text = "single line".to_string();
    input.cursor = 2;
    feed_str(&mut vim, &mut input, "dd");
    assert_eq!(input.text, "");
    assert_eq!(input.cursor, 0);

    // dd multiline with count: 2dd
    input.text = "l1\nl2\nl3\nl4".to_string();
    input.cursor = 3; // 'l2'
    feed_str(&mut vim, &mut input, "2dd");
    assert_eq!(input.text, "l1\nl4");

    // cw: changes word, enters Insert mode, leaves trailing space
    input.text = "hello world".to_string();
    input.cursor = 0;
    feed_str(&mut vim, &mut input, "cw");
    assert_eq!(input.text, " world");
    assert_eq!(input.cursor, 0);
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(vim.registers, "hello");
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // x and s
    input.text = "hello".to_string();
    input.cursor = 1;
    feed_str(&mut vim, &mut input, "2x");
    assert_eq!(input.text, "hlo");
    assert_eq!(input.cursor, 1);
    assert_eq!(vim.registers, "el");

    feed_str(&mut vim, &mut input, "s");
    assert_eq!(input.text, "ho");
    assert_eq!(input.cursor, 1);
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(vim.registers, "l");
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // D and C
    input.text = "foo bar".to_string();
    input.cursor = 3;
    feed_str(&mut vim, &mut input, "D");
    assert_eq!(input.text, "foo");

    input.text = "foo bar".to_string();
    input.cursor = 3;
    feed_str(&mut vim, &mut input, "C");
    assert_eq!(input.text, "foo");
    assert_eq!(vim.mode, VimMode::Insert);
}

#[test]
fn paste_and_yank() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "ac".to_string(),
        ..Default::default()
    };

    // Character paste: p and P
    vim.registers = "b".to_string();
    handle(&mut vim, &mut input, char_key('p'));
    assert_eq!(input.text, "abc");
    assert_eq!(input.cursor, 1);

    input.text = "ac".to_string();
    input.cursor = 1;
    handle(&mut vim, &mut input, char_key('P'));
    assert_eq!(input.text, "abc");
    assert_eq!(input.cursor, 1);

    // Line yank (Y) and line paste (p / P)
    input.text = "line1\nline2".to_string();
    input.cursor = 0;
    handle(&mut vim, &mut input, char_key('Y'));
    assert_eq!(vim.registers, "line1\n");

    handle(&mut vim, &mut input, char_key('p'));
    assert_eq!(input.text, "line1\nline1\nline2");
}

#[test]
fn undo_stack_and_depth_limit() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "initial".to_string(),
        cursor: 0,
        ..Default::default()
    };

    // Perform a change and undo it
    feed_str(&mut vim, &mut input, "dw");
    assert_eq!(input.text, "");
    handle(&mut vim, &mut input, char_key('u'));
    assert_eq!(input.text, "initial");

    // Perform 35 mutations to exceed bounded capacity (32)
    input.text = "0123456789012345678901234567890123456789".to_string();
    input.cursor = 0;
    for _ in 0..35 {
        handle(&mut vim, &mut input, char_key('x'));
    }
    assert_eq!(vim.undo_stack.len(), 32);

    // Undo 35 times: should only undo 32 times and stop gracefully
    for _ in 0..35 {
        handle(&mut vim, &mut input, char_key('u'));
    }
    assert!(vim.undo_stack.is_empty());
}

#[test]
fn visual_mode_operations() {
    let mut vim = VimState::default();
    let mut input = InputState {
        text: "hello world".to_string(),
        cursor: 0,
        ..Default::default()
    };

    // Select "hello" via visual mode
    handle(&mut vim, &mut input, char_key('v'));
    assert_eq!(vim.mode, VimMode::Visual);
    feed_str(&mut vim, &mut input, "4l");
    assert_eq!(input.cursor, 4);

    // Delete selection with 'd'
    handle(&mut vim, &mut input, char_key('d'));
    assert_eq!(vim.mode, VimMode::Normal);
    assert_eq!(input.text, " world");
    assert_eq!(vim.registers, "hello");
    assert_eq!(input.cursor, 0);

    // Undo restore
    handle(&mut vim, &mut input, char_key('u'));
    assert_eq!(input.text, "hello world");

    // Select and change with 'c'
    handle(&mut vim, &mut input, char_key('v'));
    feed_str(&mut vim, &mut input, "4l");
    handle(&mut vim, &mut input, char_key('c'));
    assert_eq!(vim.mode, VimMode::Insert);
    assert_eq!(input.text, " world");
    handle(&mut vim, &mut input, key(KeyCode::Esc));

    // Visual yank with 'y'
    input.text = "abcdef".to_string();
    input.cursor = 1;
    handle(&mut vim, &mut input, char_key('v'));
    feed_str(&mut vim, &mut input, "2l"); // b, c, d
    handle(&mut vim, &mut input, char_key('y'));
    assert_eq!(vim.mode, VimMode::Normal);
    assert_eq!(input.text, "abcdef");
    assert_eq!(vim.registers, "bcd");
}
