//! Width and wrapping by graphemes (`unicode-segmentation`) and display width
//! (`unicode-width`). Cyrillic in narrow columns must wrap by grapheme, never by byte.

use ratatui::text::Line;

/// Display width in terminal cells.
pub fn display_width(s: &str) -> usize {
    let _ = s;
    todo!("wrap::display_width")
}

/// Cut to `width` cells; when `ellipsis` and the text was cut, the last cell(s) show `…`.
pub fn truncate_to_width(s: &str, width: usize, ellipsis: bool) -> String {
    let _ = (s, width, ellipsis);
    todo!("wrap::truncate_to_width")
}

/// Word-wrap plain text to `width` cells. Words longer than the width are split by
/// grapheme. Existing newlines are respected. Never returns an empty vec.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let _ = (text, width);
    todo!("wrap::wrap_text")
}

/// Wrap a styled line, preserving span styles across the break. Continuation lines get
/// `indent` leading spaces.
pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let _ = (line, width, indent);
    todo!("wrap::wrap_line")
}
