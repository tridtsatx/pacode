//! The prompt's word-aware wrap — the one wrap the layout height, the drawn
//! rows, the cursor and the vertical moves all share.

use pacode_render::display_width;
use unicode_segmentation::UnicodeSegmentation;

/// Wrap text into lines preserving all characters without trimming.
/// Trailing whitespace (spaces, etc.) occupies terminal cells.
///
/// Word-aware (spec §4): graphemes group into words separated by whitespace
/// runs, and a line wraps at the last word boundary that still fits `width`.
/// Whitespace keeps occupying cells — a run that overflows continues at the
/// next line's start. A word longer than `width` hard-breaks mid-word.
pub fn wrap_text_non_trimming(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();

    for raw_line in text.split('\n') {
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut cur_line = String::new();
        let mut cur_w = 0usize;
        // Non-whitespace run accumulated until the next whitespace grapheme
        // (or the end of the raw line) marks it a complete word.
        let mut word = String::new();
        let mut word_w = 0usize;

        for g in raw_line.graphemes(true) {
            if g.chars().all(char::is_whitespace) {
                flush_word(
                    &mut lines,
                    &mut cur_line,
                    &mut cur_w,
                    &mut word,
                    &mut word_w,
                    width,
                );
                let gw = display_width(g);
                if cur_w + gw > width && cur_w > 0 {
                    lines.push(std::mem::take(&mut cur_line));
                    cur_w = 0;
                }
                cur_line.push_str(g);
                cur_w += gw;
            } else {
                word.push_str(g);
                word_w += display_width(g);
            }
        }
        flush_word(
            &mut lines,
            &mut cur_line,
            &mut cur_w,
            &mut word,
            &mut word_w,
            width,
        );
        lines.push(cur_line);
    }

    lines
}

/// Place a completed word on the current line: whole when it still fits, on a
/// fresh line when it only fits at a boundary, chunked mid-word when it is
/// longer than a whole line.
fn flush_word(
    lines: &mut Vec<String>,
    cur_line: &mut String,
    cur_w: &mut usize,
    word: &mut String,
    word_w: &mut usize,
    width: usize,
) {
    if word.is_empty() {
        return;
    }
    // Wrap at the word boundary first: the word starts on a fresh line.
    if *cur_w + *word_w > width && *cur_w > 0 {
        lines.push(std::mem::take(cur_line));
        *cur_w = 0;
    }
    let taken = std::mem::take(word);
    *word_w = 0;
    for g in taken.graphemes(true) {
        let gw = display_width(g);
        if *cur_w + gw > width && *cur_w > 0 {
            lines.push(std::mem::take(cur_line));
            *cur_w = 0;
        }
        cur_line.push_str(g);
        *cur_w += gw;
    }
}
