//! Pacman activity animation (spec §3, Arch ILoveCandy style).
//!
//! While `turn_active` (main agent thinking or running a tool), an animated status
//! line is rendered as the last line of the dialog area:
//! `C` moving right over candies, alternating `C`/`c` each frame.

use ratatui::style::Color;
use ratatui::text::{Line, Span};

use crate::state::activity::{Phase, displayed_secs, format_activity_secs, thinking_color};
use pacode_render::{Glyphs, RenderOptions, truncate_to_width};

/// Display width of the pacman bar, including its brackets.
const PACMAN_WIDTH: usize = 24;

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

/// Renders the activity line for `phase`.
///
/// The pacman bar is drawn only while the model itself is working; waiting on a
/// subagent or a background task gets a plain line. The duration is whole seconds,
/// so a redraw between ticks shows the same value as the tick before it.
pub fn render_activity_line(
    frame: u64,
    opts: &RenderOptions,
    phase: &Phase,
    elapsed_ms: u64,
    max_line_width: usize,
) -> Line<'static> {
    let secs = displayed_secs(elapsed_ms);
    let dur_str = format_activity_secs(secs);
    let text_style = match phase {
        Phase::Thinking => {
            let from = opts.theme.fg.fg.unwrap_or(Color::White);
            let to = opts.theme.yellow.fg.unwrap_or(Color::Yellow);
            opts.theme.fg.fg(thinking_color(elapsed_ms, from, to))
        }
        Phase::Responding | Phase::Tool(_) => opts.theme.fg,
        Phase::WaitingAgent(_) | Phase::WaitingTask(_) => opts.theme.faint,
    };

    if !phase.shows_pacman() {
        let tail = format!("{} · {dur_str}", phase.label(elapsed_ms));
        let trunc = truncate_to_width(&tail, max_line_width, true);
        return Line::from(Span::styled(trunc, text_style));
    }

    let mut spans = pacman_spans(frame, PACMAN_WIDTH, opts);
    let used_w = PACMAN_WIDTH;
    let tail = format!(" {} · {dur_str}", phase.label(elapsed_ms));
    let avail = max_line_width.saturating_sub(used_w);
    spans.push(Span::styled(
        truncate_to_width(&tail, avail, true),
        text_style,
    ));
    Line::from(spans)
}

/// Styled pacman bar of `width` cells: eaten track dim, pacman yellow, candies plain.
fn pacman_spans(frame: u64, width: usize, opts: &RenderOptions) -> Vec<Span<'static>> {
    let raw = pacman_frame(frame, width, &opts.glyphs);
    let candy_sym = if opts.glyphs.ascii { 'o' } else { '•' };

    let mut spans = Vec::new();
    let mut buf = String::new();

    for ch in raw.chars() {
        if ch == 'C' || ch == 'c' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), opts.theme.dim));
            }
            spans.push(Span::styled(ch.to_string(), opts.theme.yellow));
        } else if ch == candy_sym {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), opts.theme.dim));
            }
            spans.push(Span::styled(ch.to_string(), opts.theme.fg));
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, opts.theme.dim));
    }
    spans
}

/// Dynamic animation frame interval based on stream backlog and draining phase.
///
/// - Idle thinking: 240 ms / frame.
/// - Once stream arrives: clamp(240 - backlog_chars / 8, 60, 240) ms.
/// - When draining final item: 60 ms.
pub fn pacman_interval_ms(backlog_chars: usize, draining: bool) -> u64 {
    if draining {
        60
    } else {
        let reduction = (backlog_chars / 8) as u64;
        240u64.saturating_sub(reduction).clamp(60, 240)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_render::display_width;

    #[test]
    fn test_pacman_interval_ms() {
        // Draining is always 60 ms
        assert_eq!(pacman_interval_ms(0, true), 60);
        assert_eq!(pacman_interval_ms(500, true), 60);
        assert_eq!(pacman_interval_ms(5000, true), 60);

        // Not draining: clamp(240 - backlog/8, 60, 240)
        assert_eq!(pacman_interval_ms(0, false), 240);
        assert_eq!(pacman_interval_ms(80, false), 230);
        assert_eq!(pacman_interval_ms(800, false), 140);
        assert_eq!(pacman_interval_ms(1440, false), 60);
        assert_eq!(pacman_interval_ms(2000, false), 60);
        assert_eq!(pacman_interval_ms(usize::MAX, false), 60);
    }

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
