//! UI-free editing model for prompt vim mode.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::input::{InputState, char_to_byte_index};

#[cfg(test)]
#[path = "vim_tests.rs"]
mod vim_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
}

pub struct VimState {
    pub mode: VimMode,
    pub pending: String,
    pub count: Option<usize>,
    pub visual_anchor: Option<usize>,
    pub last_find: Option<(char, char)>,
    pub registers: String,
    pub undo_stack: VecDeque<(String, usize)>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            mode: VimMode::Normal,
            pending: String::new(),
            count: None,
            visual_anchor: None,
            last_find: None,
            registers: String::new(),
            undo_stack: VecDeque::with_capacity(32),
        }
    }
}
impl VimState {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VimEffect {
    Consumed,
    PassThrough,
    Submit,
}

pub fn handle(vim: &mut VimState, input: &mut InputState, key: KeyEvent) -> VimEffect {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return VimEffect::PassThrough;
    }

    match vim.mode {
        VimMode::Insert => handle_insert(vim, input, key),
        VimMode::Normal => handle_normal(vim, input, key),
        VimMode::Visual => handle_visual(vim, input, key),
    }
}

fn handle_insert(vim: &mut VimState, input: &mut InputState, key: KeyEvent) -> VimEffect {
    if key.code == KeyCode::Esc {
        vim.mode = VimMode::Normal;
        let chars: Vec<char> = input.text.chars().collect();
        let ls = line_start_index(&chars, input.cursor);
        if input.cursor > ls {
            input.cursor -= 1;
        }
        input.cursor = clamp_cursor(&input.text, input.cursor, false);
        return VimEffect::Consumed;
    }
    VimEffect::PassThrough
}

fn handle_visual(vim: &mut VimState, input: &mut InputState, key: KeyEvent) -> VimEffect {
    if key.code == KeyCode::Esc || matches!(key.code, KeyCode::Char('v')) {
        vim.mode = VimMode::Normal;
        vim.visual_anchor = None;
        vim.count = None;
        return VimEffect::Consumed;
    }
    let KeyCode::Char(c) = key.code else {
        return VimEffect::Consumed;
    };

    if c.is_ascii_digit() {
        if c == '0' && vim.count.is_none() {
            let chars: Vec<char> = input.text.chars().collect();
            input.cursor = line_start_index(&chars, input.cursor);
            return VimEffect::Consumed;
        }
        let digit = (c as u8 - b'0') as usize;
        let cur = vim.count.unwrap_or(0);
        vim.count = Some(cur.saturating_mul(10).saturating_add(digit));
        return VimEffect::Consumed;
    }

    let anchor = vim.visual_anchor.unwrap_or(input.cursor);
    let chars: Vec<char> = input.text.chars().collect();
    let (start, end) = (
        anchor.min(input.cursor),
        (anchor.max(input.cursor) + 1).min(chars.len()),
    );

    match c {
        'd' | 'x' | 'c' | 's' | 'y' => {
            vim.count = None;
            if c != 'y' {
                push_undo_at(vim, input, start);
            }
            vim.registers = chars[start..end].iter().collect();
            if c != 'y' {
                replace_char_range(input, start, end, "");
            }
            if matches!(c, 'c' | 's') {
                input.cursor = start;
                vim.mode = VimMode::Insert;
            } else {
                input.cursor = clamp_cursor(&input.text, start, false);
                vim.mode = VimMode::Normal;
            }
            vim.visual_anchor = None;
        }
        'p' | 'P' => {
            vim.count = None;
            push_undo_at(vim, input, start);
            let reg = vim.registers.clone();
            replace_char_range(input, start, end, &reg);
            input.cursor = clamp_cursor(&input.text, start + reg.chars().count(), false);
            vim.mode = VimMode::Normal;
            vim.visual_anchor = None;
        }
        'u' => {
            vim.count = None;
            undo(vim, input);
            vim.mode = VimMode::Normal;
            vim.visual_anchor = None;
        }
        _ => {
            let count = vim.count.take().unwrap_or(1);
            if let Some((target, _)) = resolve_motion(&chars, input.cursor, c, count, false) {
                input.cursor = clamp_cursor(&input.text, target, true);
            }
        }
    }
    VimEffect::Consumed
}

fn handle_normal(vim: &mut VimState, input: &mut InputState, key: KeyEvent) -> VimEffect {
    if key.code == KeyCode::Esc {
        vim.pending.clear();
        vim.count = None;
        return VimEffect::Consumed;
    }
    if key.code == KeyCode::Enter {
        return VimEffect::Submit;
    }
    let KeyCode::Char(c) = key.code else {
        return VimEffect::Consumed;
    };

    // 1. Pending find character: 'f', 'F', 't', 'T'
    if is_find_pending(&vim.pending) {
        return handle_find_char(vim, input, c);
    }

    // 2. Digits
    if c.is_ascii_digit() {
        if c == '0' && vim.count.is_none() && !vim.pending.ends_with(['d', 'c', 'y']) {
            let chars: Vec<char> = input.text.chars().collect();
            return apply_motion(vim, input, line_start_index(&chars, input.cursor), false);
        }
        let digit = (c as u8 - b'0') as usize;
        let cur = vim.count.unwrap_or(0);
        vim.count = Some(cur.saturating_mul(10).saturating_add(digit));
        return VimEffect::Consumed;
    }

    // 3. 'g' prefix for 'gg'
    if c == 'g' {
        if vim.pending.ends_with('g') {
            // The pending buffer holds "<count>g" (and possibly a leading
            // operator). Strip it back to the operator alone, otherwise
            // `apply_motion` reads the leftover "g" as an operator and deletes.
            let count = pending_digits(&vim.pending)
                .unwrap_or(1)
                .saturating_mul(vim.count.take().unwrap_or(1));
            let op = vim
                .pending
                .chars()
                .find(|&ch| matches!(ch, 'd' | 'c' | 'y'));
            vim.pending = op.map(String::from).unwrap_or_default();
            let chars: Vec<char> = input.text.chars().collect();
            let target = line_index_start(&chars, count.saturating_sub(1));
            return apply_motion(vim, input, target, false);
        }
        let op_prefix = if vim.pending.is_empty() {
            String::new()
        } else {
            std::mem::take(&mut vim.pending)
        };
        let op_count = vim.count.take().unwrap_or(1);
        vim.pending = format!("{op_prefix}{op_count}g");
        return VimEffect::Consumed;
    }

    // 4. Mode switching
    if vim.pending.is_empty() && matches!(c, 'i' | 'a' | 'I' | 'A' | 'o' | 'O' | 'v') {
        vim.count = None;
        match c {
            'i' => {
                push_undo(vim, input);
                vim.mode = VimMode::Insert;
            }
            'a' => {
                push_undo(vim, input);
                input.move_right();
                vim.mode = VimMode::Insert;
            }
            'I' => {
                push_undo(vim, input);
                input.cursor =
                    line_first_non_blank(&input.text.chars().collect::<Vec<_>>(), input.cursor);
                vim.mode = VimMode::Insert;
            }
            'A' => {
                push_undo(vim, input);
                input.cursor =
                    line_end_index(&input.text.chars().collect::<Vec<_>>(), input.cursor);
                vim.mode = VimMode::Insert;
            }
            'o' | 'O' => {
                push_undo(vim, input);
                let chars: Vec<char> = input.text.chars().collect();
                let idx = if c == 'o' {
                    line_end_index(&chars, input.cursor)
                } else {
                    line_start_index(&chars, input.cursor)
                };
                input
                    .text
                    .insert(char_to_byte_index(&input.text, idx), '\n');
                input.cursor = if c == 'o' { idx + 1 } else { idx };
                vim.mode = VimMode::Insert;
            }
            'v' => {
                vim.mode = VimMode::Visual;
                vim.visual_anchor = Some(input.cursor);
            }
            _ => return VimEffect::Consumed,
        }
        return VimEffect::Consumed;
    }

    // 5. Single-key operators
    if vim.pending.is_empty() && matches!(c, 'x' | 's' | 'D' | 'C' | 'Y' | 'p' | 'P' | 'u') {
        let count = vim.count.take().unwrap_or(1);
        match c {
            'x' | 's' => {
                vim.pending = (if c == 'x' { "d" } else { "c" }).to_string();
                let end = (input.cursor + count).min(input.text.chars().count());
                return apply_motion(vim, input, end, false);
            }
            'D' | 'C' => {
                vim.pending = (if c == 'D' { "d" } else { "c" }).to_string();
                let chars: Vec<char> = input.text.chars().collect();
                return apply_motion(vim, input, line_end_index(&chars, input.cursor), false);
            }
            'Y' => {
                execute_line_op(vim, input, LineOp::Yank, count);
            }
            'p' => paste(vim, input, true),
            'P' => paste(vim, input, false),
            'u' => {
                for _ in 0..count {
                    undo(vim, input);
                }
            }
            _ => {}
        }
        return VimEffect::Consumed;
    }

    // 6. Operators: d, c, y (doubled: dd, cc, yy)
    if matches!(c, 'd' | 'c' | 'y') {
        if let Some(op) = vim.pending.chars().last()
            && op == c
        {
            let op_count = parse_leading_count(&vim.pending).unwrap_or(1);
            let motion_count = vim.count.take().unwrap_or(1);
            let total = op_count.saturating_mul(motion_count);
            vim.pending.clear();
            let line_op = match c {
                'd' => LineOp::Delete,
                'c' => LineOp::Change,
                _ => LineOp::Yank,
            };
            execute_line_op(vim, input, line_op, total);
            return VimEffect::Consumed;
        }
        if vim.pending.is_empty() {
            let count_val = vim.count.take().unwrap_or(1);
            vim.pending = format!("{count_val}{c}");
            return VimEffect::Consumed;
        }
    }

    // 7. Find commands: f, F, t, T
    if matches!(c, 'f' | 'F' | 't' | 'T') {
        let count = vim.count.take().unwrap_or(1);
        vim.pending.push_str(&format!("{count}{c}"));
        return VimEffect::Consumed;
    }

    // 8. Find repeat: ; and ,
    let total_count = parse_leading_count(&vim.pending)
        .unwrap_or(1)
        .saturating_mul(vim.count.take().unwrap_or(1));
    let chars: Vec<char> = input.text.chars().collect();
    if c == ';' || c == ',' {
        if let Some((cmd, target_c)) = vim.last_find {
            let actual_cmd = if c == ',' { reverse_find(cmd) } else { cmd };
            if let Some(target) =
                resolve_find(&chars, input.cursor, actual_cmd, target_c, total_count)
            {
                return apply_motion(vim, input, target, matches!(actual_cmd, 'f' | 't'));
            }
        }
        return VimEffect::Consumed;
    }

    // 9. Standard motions
    let is_cw = c == 'w' && vim.pending.ends_with('c');
    if let Some((target, inclusive)) = resolve_motion(&chars, input.cursor, c, total_count, is_cw) {
        return apply_motion(vim, input, target, inclusive);
    }

    VimEffect::Consumed
}

/// Digits held anywhere in the pending buffer (`2g`, `d2g`), as a count.
fn pending_digits(pending: &str) -> Option<usize> {
    let digits: String = pending.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn is_find_pending(pending: &str) -> bool {
    pending.ends_with('f')
        || pending.ends_with('F')
        || pending.ends_with('t')
        || pending.ends_with('T')
}

fn handle_find_char(vim: &mut VimState, input: &mut InputState, c: char) -> VimEffect {
    let pending = std::mem::take(&mut vim.pending);
    let chars: Vec<char> = pending.chars().collect();
    let find_type = *chars.last().unwrap_or(&'f');
    vim.last_find = Some((find_type, c));

    let (op, count) = parse_op_and_find_count(&pending);
    if let Some(op_char) = op {
        vim.pending = format!("{op_char}");
    }
    let input_chars: Vec<char> = input.text.chars().collect();
    if let Some(target) = resolve_find(&input_chars, input.cursor, find_type, c, count) {
        let inclusive = matches!(find_type, 'f' | 't');
        apply_motion(vim, input, target, inclusive)
    } else {
        vim.pending.clear();
        VimEffect::Consumed
    }
}

fn apply_motion(
    vim: &mut VimState,
    input: &mut InputState,
    target: usize,
    inclusive: bool,
) -> VimEffect {
    if vim.pending.is_empty() {
        input.cursor = clamp_cursor(&input.text, target, false);
        return VimEffect::Consumed;
    }
    let op = vim
        .pending
        .chars()
        .find(|&ch| ch == 'd' || ch == 'c' || ch == 'y')
        .unwrap_or('d');
    vim.pending.clear();
    let chars: Vec<char> = input.text.chars().collect();
    let (start, end) = (
        input.cursor.min(target),
        if inclusive {
            (input.cursor.max(target) + 1).min(chars.len())
        } else {
            input.cursor.max(target).min(chars.len())
        },
    );
    if op != 'y' {
        push_undo_at(vim, input, start);
    }
    vim.registers = chars[start..end].iter().collect();
    if op != 'y' {
        replace_char_range(input, start, end, "");
    }
    if op == 'c' {
        input.cursor = start;
        vim.mode = VimMode::Insert;
    } else if op == 'd' {
        input.cursor = clamp_cursor(&input.text, start, false);
    }
    VimEffect::Consumed
}

enum LineOp {
    Delete,
    Change,
    Yank,
}

fn execute_line_op(vim: &mut VimState, input: &mut InputState, op: LineOp, count: usize) {
    let chars: Vec<char> = input.text.chars().collect();
    let (ls, le) = get_line_range(&chars, input.cursor, count);
    vim.registers = chars[ls..le].iter().collect();
    if !vim.registers.ends_with('\n') {
        vim.registers.push('\n');
    }
    match op {
        LineOp::Yank => {}
        LineOp::Delete => {
            push_undo(vim, input);
            replace_char_range(input, ls, le, "");
            input.cursor = clamp_cursor(&input.text, ls, false);
        }
        LineOp::Change => {
            push_undo(vim, input);
            replace_char_range(input, ls, line_end_index(&chars, input.cursor), "");
            input.cursor = ls;
            vim.mode = VimMode::Insert;
        }
    }
}

fn paste(vim: &mut VimState, input: &mut InputState, after: bool) {
    if vim.registers.is_empty() {
        return;
    }
    push_undo(vim, input);
    let chars: Vec<char> = input.text.chars().collect();
    if vim.registers.ends_with('\n') {
        let (idx, off, text) = if after {
            let le = line_end_index(&chars, input.cursor);
            let trimmed = vim.registers.trim_end_matches('\n');
            if le < chars.len() && chars[le] == '\n' {
                (le, 1, vim.registers.clone())
            } else {
                (le, 0, format!("\n{trimmed}"))
            }
        } else {
            (
                line_start_index(&chars, input.cursor),
                0,
                vim.registers.clone(),
            )
        };
        input
            .text
            .insert_str(char_to_byte_index(&input.text, idx) + off, &text);
        input.cursor = idx + off;
    } else if chars.is_empty() {
        input.text = vim.registers.clone();
        input.cursor = 0;
    } else {
        let idx = if after {
            (input.cursor + 1).min(chars.len())
        } else {
            input.cursor
        };
        input
            .text
            .insert_str(char_to_byte_index(&input.text, idx), &vim.registers);
        input.cursor = idx;
    }
}

fn push_undo(vim: &mut VimState, input: &InputState) {
    push_undo_at(vim, input, input.cursor);
}

/// Undo entries remember where the change STARTS, not where the cursor happened
/// to be: vim puts the cursor at the start of restored text, and a visual
/// operator runs with the cursor at the far end of the selection.
fn push_undo_at(vim: &mut VimState, input: &InputState, cursor: usize) {
    if vim.undo_stack.len() >= 32 {
        vim.undo_stack.pop_front();
    }
    vim.undo_stack.push_back((input.text.clone(), cursor));
}
fn undo(vim: &mut VimState, input: &mut InputState) {
    if let Some((prev_text, prev_cursor)) = vim.undo_stack.pop_back() {
        input.text = prev_text;
        input.cursor = prev_cursor.min(input.text.chars().count().saturating_sub(1));
    }
}
fn replace_char_range(input: &mut InputState, start: usize, end: usize, rep: &str) {
    let sb = char_to_byte_index(&input.text, start);
    let eb = char_to_byte_index(&input.text, end);
    input.text.replace_range(sb..eb, rep);
}
fn clamp_cursor(text: &str, cursor: usize, in_vis_ins: bool) -> usize {
    let count = text.chars().count();
    if count == 0 || in_vis_ins {
        return cursor.min(count);
    }
    let c = cursor.min(count - 1);
    let chars: Vec<char> = text.chars().collect();
    if chars[c] == '\n' && c > line_start_index(&chars, c) {
        c - 1
    } else {
        c
    }
}
fn line_start_index(chars: &[char], pos: usize) -> usize {
    (0..=pos.min(chars.len().saturating_sub(1)))
        .rev()
        .find(|&i| i == 0 || chars[i - 1] == '\n')
        .unwrap_or(0)
}
fn line_end_index(chars: &[char], pos: usize) -> usize {
    (pos.min(chars.len())..chars.len())
        .find(|&i| chars[i] == '\n')
        .unwrap_or(chars.len())
}
fn line_first_non_blank(chars: &[char], pos: usize) -> usize {
    let ls = line_start_index(chars, pos);
    (ls..line_end_index(chars, pos))
        .find(|&i| !chars[i].is_whitespace())
        .unwrap_or(ls)
}
fn line_index_start(chars: &[char], line_idx: usize) -> usize {
    if line_idx == 0 {
        return 0;
    }
    chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '\n')
        .nth(line_idx - 1)
        .map(|(i, _)| i + 1)
        .unwrap_or(chars.len())
}
fn get_line_range(chars: &[char], cursor: usize, count: usize) -> (usize, usize) {
    let ls = line_start_index(chars, cursor);
    let mut le = line_end_index(chars, cursor);
    for _ in 1..count {
        if le < chars.len() && chars[le] == '\n' {
            le = line_end_index(chars, le + 1);
        }
    }
    if le < chars.len() && chars[le] == '\n' {
        (ls, le + 1)
    } else if ls > 0 && chars[ls - 1] == '\n' {
        (ls - 1, le)
    } else {
        (ls, le)
    }
}

fn resolve_motion(
    chars: &[char],
    cursor: usize,
    c: char,
    count: usize,
    is_cw: bool,
) -> Option<(usize, bool)> {
    match c {
        'h' => Some((
            cursor
                .saturating_sub(count)
                .max(line_start_index(chars, cursor)),
            false,
        )),
        'l' => {
            let le = line_end_index(chars, cursor);
            let max_c = le.saturating_sub(1).max(line_start_index(chars, cursor));
            Some(((cursor + count).min(max_c), false))
        }
        'j' | 'k' => {
            let down = c == 'j';
            let starts: Vec<usize> = std::iter::once(0)
                .chain(
                    chars
                        .iter()
                        .enumerate()
                        .filter(|(_, ch)| **ch == '\n')
                        .map(|(i, _)| i + 1),
                )
                .collect();
            let cur = match starts.binary_search(&cursor) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let col = cursor.saturating_sub(starts[cur]);
            let target_line = if down {
                (cur + count).min(starts.len() - 1)
            } else {
                cur.saturating_sub(count)
            };
            let ls = starts[target_line];
            let max_col = line_end_index(chars, ls)
                .saturating_sub(ls)
                .saturating_sub(1);
            Some((ls + col.min(max_col), false))
        }
        'w' | 'W' => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = if is_cw {
                    next_word_end(chars, pos, c == 'W')
                } else {
                    next_word_start(chars, pos, c == 'W')
                };
            }
            Some((pos, is_cw))
        }
        'b' | 'B' => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = prev_word_start(chars, pos, c == 'B');
            }
            Some((pos, false))
        }
        'e' | 'E' => {
            let mut pos = cursor;
            for _ in 0..count {
                pos = next_word_end(chars, pos, c == 'E');
            }
            Some((pos, true))
        }
        '0' => Some((line_start_index(chars, cursor), false)),
        '^' => Some((line_first_non_blank(chars, cursor), false)),
        '$' => Some((line_end_index(chars, cursor), false)),
        'G' => {
            let line = if count > 1 {
                count - 1
            } else {
                chars.iter().filter(|&&ch| ch == '\n').count()
            };
            Some((line_index_start(chars, line), false))
        }
        _ => None,
    }
}

fn resolve_find(chars: &[char], cursor: usize, cmd: char, tc: char, count: usize) -> Option<usize> {
    let (fwd, inc) = match cmd {
        'f' => (true, 0),
        't' => (true, 1),
        'F' => (false, 0),
        _ => (false, 1),
    };
    let nth = count.saturating_sub(1);
    if fwd {
        ((cursor + 1)..line_end_index(chars, cursor))
            .filter(|&i| chars[i] == tc)
            .nth(nth)
            .map(|p| p.saturating_sub(inc))
    } else {
        (line_start_index(chars, cursor)..cursor)
            .rev()
            .filter(|&i| chars[i] == tc)
            .nth(nth)
            .map(|p| p + inc)
    }
}
fn reverse_find(cmd: char) -> char {
    match cmd {
        'f' => 'F',
        'F' => 'f',
        't' => 'T',
        _ => 't',
    }
}

fn char_class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}
fn next_word_start(chars: &[char], mut pos: usize, big: bool) -> usize {
    if pos >= chars.len() {
        return chars.len();
    }
    let cls = char_class(chars[pos], big);
    if cls != 0 {
        while pos < chars.len() && char_class(chars[pos], big) == cls {
            pos += 1;
        }
    }
    while pos < chars.len() && char_class(chars[pos], big) == 0 {
        pos += 1;
    }
    pos
}
fn prev_word_start(chars: &[char], mut pos: usize, big: bool) -> usize {
    if pos == 0 || chars.is_empty() {
        return 0;
    }
    pos = pos.min(chars.len() - 1).saturating_sub(1);
    while pos > 0 && char_class(chars[pos], big) == 0 {
        pos -= 1;
    }
    let cls = char_class(chars[pos], big);
    if cls != 0 {
        while pos > 0 && char_class(chars[pos - 1], big) == cls {
            pos -= 1;
        }
    }
    pos
}
fn next_word_end(chars: &[char], mut pos: usize, big: bool) -> usize {
    if chars.is_empty() || pos + 1 >= chars.len() {
        return chars.len().saturating_sub(1);
    }
    pos += 1;
    while pos < chars.len() && char_class(chars[pos], big) == 0 {
        pos += 1;
    }
    if pos >= chars.len() {
        return chars.len().saturating_sub(1);
    }
    let cls = char_class(chars[pos], big);
    while pos + 1 < chars.len() && char_class(chars[pos + 1], big) == cls {
        pos += 1;
    }
    pos
}
fn parse_leading_count(s: &str) -> Option<usize> {
    s.chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}
fn parse_op_and_find_count(s: &str) -> (Option<char>, usize) {
    let op = s.chars().find(|&c| c == 'd' || c == 'c' || c == 'y');
    let Some(op_c) = op else {
        return (
            None,
            s.chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(1),
        );
    };
    let parts: Vec<&str> = s.split(op_c).collect();
    let c1 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(1);
    let c2 = parts
        .get(1)
        .unwrap_or(&"")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(1);
    (Some(op_c), c1 * c2)
}
