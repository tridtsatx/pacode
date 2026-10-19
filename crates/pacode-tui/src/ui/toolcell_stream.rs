//! The body of a tool call that is still running: the bytes received so far,
//! drawn on the same gutter, with the same fold, the same expansion rule and
//! the same ANSI stripping a finished body gets — plus a cursor on the line
//! that is still arriving.
//!
//! The last line is the point. A live body whose tail looks complete reads as
//! a finished call, and the reader stops watching the one row in the
//! transcript that is still moving.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::transcript::ToolStatus;

use super::{gutter, push_collapsed, push_expanded};
use crate::state::transcript::MAX_TOOL_STREAM_CHARS;

/// Append the body of a running call: `text` is what has arrived, `dropped`
/// how much of it the streaming cap keeps out of the cell.
pub(super) fn push_live_body(
    out: &mut Vec<Line<'static>>,
    text: &str,
    dropped: usize,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
    expanded: bool,
) {
    if text.is_empty() {
        return;
    }
    if dropped > 0 {
        out.push(Line::from(vec![
            gutter(opts, gutter_style),
            Span::styled(cap_marker(dropped, opts), opts.theme.faint),
        ]));
    }

    // A trailing newline leaves the next line unwritten: that empty line is
    // the one still arriving, so the cursor stands on it rather than on the
    // row above, which really is finished.
    let mut lines: Vec<&str> = text.lines().collect();
    if text.ends_with('\n') {
        lines.push("");
    }

    // The ok fold (first lines, last lines, marker between) rather than the
    // failure one: a call has no outcome yet, and a live body is read from
    // both ends — the head for context, the tail for progress.
    let last = if expanded {
        push_expanded(out, &lines, body_w, gutter_style, opts)
    } else {
        push_collapsed(out, ToolStatus::Ok, &lines, body_w, gutter_style, opts)
    };
    if let Some(tail) = last.and_then(|index| out.get_mut(index)) {
        mark_tail(tail, body_w, opts);
    }
}

/// Stand the cursor on `tail`, taking the columns it needs from that row: a
/// body row is already `body_w` wide, and a cursor pushed past the cell edge
/// is clipped off screen — the one row that must not lose its marker.
fn mark_tail(tail: &mut Line<'static>, body_w: usize, opts: &RenderOptions) {
    let cursor = live_cursor(opts);
    let room = body_w.saturating_sub(display_width(cursor));
    // A row of one span is the gutter alone: the line the cursor is already
    // standing on, with nothing to make room in.
    if tail.spans.len() > 1
        && let Some(span) = tail.spans.last_mut()
        && display_width(&span.content) > room
    {
        span.content = truncate_to_width(&span.content, room, true).into();
    }
    tail.spans.push(Span::styled(cursor, opts.theme.faint));
}

/// `↑ 3 200 characters above the 16 000-character streaming cap` — the cap is
/// never silent. Without the marker a window onto a longer output is
/// indistinguishable from the whole of it, and the reader cannot tell that
/// what scrolled off the top is still coming.
fn cap_marker(dropped: usize, opts: &RenderOptions) -> String {
    format!(
        "{} {dropped} characters above the {MAX_TOOL_STREAM_CHARS}-character streaming cap",
        opts.glyphs.arrow_up
    )
}

/// The cursor drawn on the last line of a live body. `pub(super)` so the cell
/// tests can build the same string the cell draws.
pub(super) fn live_cursor(opts: &RenderOptions) -> &'static str {
    if opts.glyphs.ascii { "_" } else { "▍" }
}
