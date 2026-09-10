//! Width and wrapping by graphemes (`unicode-segmentation`) and display width
//! (`unicode-width`). Cyrillic in narrow columns must wrap by grapheme, never by byte.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod wrap_tests;

/// Display width in terminal cells.
pub fn display_width(s: &str) -> usize {
    let mut total = 0;
    for g in s.graphemes(true) {
        if g == "\t" {
            total += 4;
        } else if g.chars().all(|ch| ch.is_control()) {
            continue;
        } else if g.contains('\u{200D}') {
            // An emoji ZWJ sequence (e.g. 👩‍💻, 👨‍👩‍👧‍👦) renders as a single 2-cell glyph.
            total += 2;
        } else {
            let mut clean = String::with_capacity(g.len());
            for ch in g.chars() {
                if !ch.is_control() {
                    clean.push(ch);
                }
            }
            total += UnicodeWidthStr::width(clean.as_str());
        }
    }
    total
}

/// Cut to `width` cells; when `ellipsis` and the text was cut, the last cell(s) show `…`.
pub fn truncate_to_width(s: &str, width: usize, ellipsis: bool) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(s) <= width {
        return s.to_string();
    }

    let target_width = if ellipsis {
        width.saturating_sub(1)
    } else {
        width
    };

    let mut out = String::new();
    let mut current_width = 0;

    for g in s.graphemes(true) {
        let gw = display_width(g);
        if current_width + gw > target_width {
            break;
        }
        out.push_str(g);
        current_width += gw;
    }

    if ellipsis {
        out.push('…');
    }

    out
}

/// Word-wrap plain text to `width` cells. Words longer than the width are split by
/// grapheme. Existing newlines are respected. Never returns an empty vec.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let lines = wrap_line(Line::from(raw_line.to_string()), width, 0);
        for l in lines {
            out.push(l.to_string());
        }
    }
    out
}

#[derive(Clone)]
struct GraphemePiece {
    text: String,
    style: Style,
    width: usize,
}

enum Token {
    Word {
        pieces: Vec<GraphemePiece>,
        total_width: usize,
    },
    Whitespace {
        pieces: Vec<GraphemePiece>,
        total_width: usize,
    },
    Newline,
}

/// Wrap a styled line, preserving span styles across the break. Continuation lines get
/// `indent` leading spaces.
pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let alignment = line.alignment;

    if line.spans.is_empty() {
        let mut l = Line::default();
        if let Some(align) = alignment {
            l = l.alignment(align);
        }
        return vec![l];
    }

    let cont_indent_len = indent.min(width.saturating_sub(1));

    let mut tokens = Vec::new();
    let mut cur_word_pieces = Vec::new();
    let mut cur_word_width = 0usize;
    let mut cur_ws_pieces = Vec::new();
    let mut cur_ws_width = 0usize;

    for span in line.spans {
        let style = span.style;
        for g in span.content.graphemes(true) {
            if g == "\n" || g == "\r\n" {
                if !cur_word_pieces.is_empty() {
                    tokens.push(Token::Word {
                        pieces: std::mem::take(&mut cur_word_pieces),
                        total_width: cur_word_width,
                    });
                    cur_word_width = 0;
                }
                if !cur_ws_pieces.is_empty() {
                    tokens.push(Token::Whitespace {
                        pieces: std::mem::take(&mut cur_ws_pieces),
                        total_width: cur_ws_width,
                    });
                    cur_ws_width = 0;
                }
                tokens.push(Token::Newline);
            } else if g.chars().all(char::is_whitespace) {
                if !cur_word_pieces.is_empty() {
                    tokens.push(Token::Word {
                        pieces: std::mem::take(&mut cur_word_pieces),
                        total_width: cur_word_width,
                    });
                    cur_word_width = 0;
                }
                let gw = display_width(g);
                cur_ws_pieces.push(GraphemePiece {
                    text: g.to_string(),
                    style,
                    width: gw,
                });
                cur_ws_width += gw;
            } else {
                if !cur_ws_pieces.is_empty() {
                    tokens.push(Token::Whitespace {
                        pieces: std::mem::take(&mut cur_ws_pieces),
                        total_width: cur_ws_width,
                    });
                    cur_ws_width = 0;
                }
                let gw = display_width(g);
                cur_word_pieces.push(GraphemePiece {
                    text: g.to_string(),
                    style,
                    width: gw,
                });
                cur_word_width += gw;
            }
        }
    }
    if !cur_word_pieces.is_empty() {
        tokens.push(Token::Word {
            pieces: cur_word_pieces,
            total_width: cur_word_width,
        });
    }
    if !cur_ws_pieces.is_empty() {
        tokens.push(Token::Whitespace {
            pieces: cur_ws_pieces,
            total_width: cur_ws_width,
        });
    }

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur_spans: Vec<Span<'static>> = Vec::new();
    let mut cur_width = 0usize;
    let mut is_continuation = false;
    let mut pending_ws: Option<Vec<GraphemePiece>> = None;
    let mut pending_ws_width = 0usize;
    let mut last_was_newline = false;

    let push_piece = |spans: &mut Vec<Span<'static>>, piece: GraphemePiece| {
        if let Some(last) = spans.last_mut()
            && last.style == piece.style
        {
            let mut s = last.content.to_string();
            s.push_str(&piece.text);
            last.content = std::borrow::Cow::Owned(s);
        } else {
            spans.push(Span::styled(piece.text, piece.style));
        }
    };

    let flush_cur_line = |out: &mut Vec<Line<'static>>,
                          cur_spans: &mut Vec<Span<'static>>,
                          cur_width: &mut usize,
                          is_continuation: &mut bool| {
        let mut l = Line::from(std::mem::take(cur_spans));
        if let Some(align) = alignment {
            l = l.alignment(align);
        }
        out.push(l);
        *cur_width = 0;
        *is_continuation = true;
    };

    for token in tokens {
        match token {
            Token::Newline => {
                pending_ws = None;
                pending_ws_width = 0;
                flush_cur_line(
                    &mut out,
                    &mut cur_spans,
                    &mut cur_width,
                    &mut is_continuation,
                );
                is_continuation = false;
                last_was_newline = true;
            }
            Token::Whitespace {
                pieces,
                total_width,
            } => {
                last_was_newline = false;
                if !is_continuation && cur_spans.is_empty() && pending_ws.is_none() {
                    pending_ws = Some(pieces);
                    pending_ws_width = total_width;
                } else if !cur_spans.is_empty() {
                    if let Some(ref mut existing) = pending_ws {
                        existing.extend(pieces);
                        pending_ws_width += total_width;
                    } else {
                        pending_ws = Some(pieces);
                        pending_ws_width = total_width;
                    }
                }
            }
            Token::Word {
                pieces,
                total_width,
            } => {
                last_was_newline = false;
                if is_continuation && cur_spans.is_empty() && cont_indent_len > 0 {
                    cur_spans.push(Span::raw(" ".repeat(cont_indent_len)));
                    cur_width = cont_indent_len;
                }

                let needed = pending_ws_width + total_width;

                if cur_width + needed <= width {
                    if let Some(ws) = pending_ws.take() {
                        for p in ws {
                            push_piece(&mut cur_spans, p);
                        }
                    }
                    pending_ws_width = 0;
                    for p in pieces {
                        push_piece(&mut cur_spans, p);
                    }
                    cur_width += needed;
                } else {
                    let line_has_content =
                        cur_width > (if is_continuation { cont_indent_len } else { 0 });
                    if line_has_content {
                        pending_ws = None;
                        pending_ws_width = 0;
                        flush_cur_line(
                            &mut out,
                            &mut cur_spans,
                            &mut cur_width,
                            &mut is_continuation,
                        );
                    } else {
                        pending_ws = None;
                        pending_ws_width = 0;
                    }

                    if is_continuation && cur_spans.is_empty() && cont_indent_len > 0 {
                        cur_spans.push(Span::raw(" ".repeat(cont_indent_len)));
                        cur_width = cont_indent_len;
                    }

                    if cur_width + total_width <= width {
                        for p in pieces {
                            push_piece(&mut cur_spans, p);
                        }
                        cur_width += total_width;
                    } else {
                        for p in pieces {
                            let gw = p.width;
                            let line_has_chars =
                                cur_width > (if is_continuation { cont_indent_len } else { 0 });
                            if line_has_chars && cur_width + gw > width {
                                flush_cur_line(
                                    &mut out,
                                    &mut cur_spans,
                                    &mut cur_width,
                                    &mut is_continuation,
                                );
                                if cont_indent_len > 0 {
                                    cur_spans.push(Span::raw(" ".repeat(cont_indent_len)));
                                    cur_width = cont_indent_len;
                                }
                            }
                            push_piece(&mut cur_spans, p);
                            cur_width += gw;
                        }
                    }
                }
            }
        }
    }

    if cur_spans.is_empty()
        && let Some(ws) = pending_ws.take()
    {
        for p in ws {
            push_piece(&mut cur_spans, p);
        }
    }
    if !cur_spans.is_empty() {
        flush_cur_line(
            &mut out,
            &mut cur_spans,
            &mut cur_width,
            &mut is_continuation,
        );
    } else if last_was_newline {
        let mut l = Line::default();
        if let Some(align) = alignment {
            l = l.alignment(align);
        }
        out.push(l);
    }

    if out.is_empty() {
        let mut l = Line::default();
        if let Some(align) = alignment {
            l = l.alignment(align);
        }
        out.push(l);
    }

    out
}
