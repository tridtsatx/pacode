//! Pacman activity animation (spec §3, Arch ILoveCandy style).
//!
//! While `turn_active` (main agent thinking or running a tool), an animated status
//! line is rendered as the last line of the dialog area:
//! `C` moving right over candies, alternating `C`/`c` each frame.

use ratatui::text::{Line, Span};

use codeapp_render::{Glyphs, RenderOptions, display_width, truncate_to_width};
use codeapp_types::time::format_duration_ms;

/// Pure function generating the pacman progress bar frame.
///
/// Format: `[  C • • • • ]` -> `[    c • • • ]`.
/// Width is the total display width including `[` and `]`.
pub fn pacman_frame(frame: u64, width: usize, glyphs: &Glyphs) -> String {
    if width < 3 {
        return "C".chars().take(width).collect();
    }

    let inner_w = width - 2;
    let candy_str = if glyphs.ascii { "o" } else { "•" };
    let mouth = if frame.is_multiple_of(2) { 'C' } else { 'c' };

    // Pacman advances by 2 cells per step (eating one candy slot: "  " or "    ").
    // Step 0: 2 spaces, then C, then candies.
    // Step 1: 4 spaces, then c, then candies.
    let num_steps = inner_w / 2;
    let step = if num_steps > 0 {
        (frame as usize) % num_steps
    } else {
        0
    };

    let spaces_count = 2 * step + 1;
    let mut out = String::with_capacity(width + 8);
    out.push('[');

    // Behind pacman: eaten cells are spaces, then pacman mouth if space permits
    if spaces_count + 1 < inner_w {
        for _ in 0..spaces_count {
            out.push(' ');
        }
        out.push(' ');
        out.push(mouth);
    } else {
        // Near the end: fill with spaces and place pacman
        let spaces = (inner_w.saturating_sub(1)).min(spaces_count);
        for _ in 0..spaces {
            out.push(' ');
        }
        if out.len() < width - 1 {
            out.push(mouth);
        }
    }

    // Ahead of pacman: alternating spaces and candies until inner_w is filled
    let mut cur_len = out.chars().count() - 1; // excluding '['
    while cur_len + 2 <= inner_w {
        out.push(' ');
        out.push_str(candy_str);
        cur_len += 2;
    }
    while cur_len < inner_w {
        out.push(' ');
        cur_len += 1;
    }

    out.push(']');
    out
}

/// Renders the animated status line: styled pacman (width 24) followed by activity and duration.
pub fn render_pacman_line(
    frame: u64,
    width: usize,
    opts: &RenderOptions,
    activity: &str,
    elapsed_ms: u64,
    max_line_width: usize,
) -> Line<'static> {
    let raw = pacman_frame(frame, width, &opts.glyphs);
    let candy_sym = if opts.glyphs.ascii { 'o' } else { '•' };

    let mut spans = Vec::new();
    let mut buf = String::new();

    for ch in raw.chars() {
        if ch == 'C' || ch == 'c' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), opts.theme.dim));
            }
            spans.push(Span::styled(ch.to_string(), opts.theme.accent));
        } else if ch == candy_sym {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), opts.theme.dim));
            }
            spans.push(Span::styled(ch.to_string(), opts.theme.faint));
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, opts.theme.dim));
    }

    // Append activity and duration
    let dur_str = format_duration_ms(elapsed_ms);
    let tail = format!(" {activity} · {dur_str}");
    let used_w = display_width(&raw);
    let avail = max_line_width.saturating_sub(used_w);
    let trunc_tail = truncate_to_width(&tail, avail, true);

    spans.push(Span::styled(trunc_tail, opts.theme.fg));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pacman_frame_ascii() {
        let glyphs = Glyphs::new(true);
        let f0 = pacman_frame(0, 14, &glyphs);
        assert_eq!(f0, "[  C o o o o ]");

        let f1 = pacman_frame(1, 14, &glyphs);
        assert_eq!(f1, "[    c o o o ]");

        let f2 = pacman_frame(2, 14, &glyphs);
        assert_eq!(f2, "[      C o o ]");
    }

    #[test]
    fn test_pacman_frame_unicode() {
        let glyphs = Glyphs::new(false);
        let f0 = pacman_frame(0, 14, &glyphs);
        assert_eq!(f0, "[  C • • • • ]");

        let f1 = pacman_frame(1, 14, &glyphs);
        assert_eq!(f1, "[    c • • • ]");
    }

    #[test]
    fn test_pacman_frame_width_24() {
        let glyphs = Glyphs::new(false);
        let f0 = pacman_frame(0, 24, &glyphs);
        assert_eq!(display_width(&f0), 24);

        let f1 = pacman_frame(1, 24, &glyphs);
        assert_eq!(display_width(&f1), 24);

        let f10 = pacman_frame(10, 24, &glyphs);
        assert_eq!(display_width(&f10), 24);
    }
}
