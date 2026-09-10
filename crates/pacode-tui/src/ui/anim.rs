//! Pacman activity animation (spec §3, Arch ILoveCandy style).
//!
//! While `turn_active` (main agent thinking or running a tool), an animated status
//! line is rendered as the last line of the dialog area:
//! `C` moving right over candies, alternating `C`/`c` each frame.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::state::activity::{Phase, displayed_secs, format_activity_secs, thinking_color};
use pacode_render::{Glyphs, RenderOptions, Theme, truncate_to_width};

/// Display width of the activity bar, including its brackets.
const PACMAN_WIDTH: usize = 24;

/// Tick interval for short-lived animations (toast slide/fade, overlay
/// unfold, welcome cascade): ~14 fps is smooth enough for a sub-200 ms move
/// and still bounded — the tick stops as soon as the window closes.
pub const TRANSIENT_TICK_MS: u64 = 70;

/// The dim half of a two-state pulse: `base` halfway to `faint` when both ends
/// are RGB, else `base` with the terminal's DIM modifier. Two states are
/// enough for a pulse — a lerp would need frames the tick does not provide.
pub fn dimmed(base: Style, faint: Style) -> Style {
    match (base.fg, faint.fg) {
        (Some(Color::Rgb(r0, g0, b0)), Some(Color::Rgb(r1, g1, b1))) => base.fg(Color::Rgb(
            ((r0 as u16 + r1 as u16) / 2) as u8,
            ((g0 as u16 + g1 as u16) / 2) as u8,
            ((b0 as u16 + b1 as u16) / 2) as u8,
        )),
        _ => base.add_modifier(Modifier::DIM),
    }
}

/// `base` while the pulse is lit, `dimmed` toward `faint` while it is not.
pub fn pulse(theme: &Theme, base: Style, lit: bool) -> Style {
    if lit { base } else { dimmed(base, theme.faint) }
}

/// Environment variable that decides which activity bar is drawn, named after
/// the `pacman.conf` option it is a nod to.
pub const CANDY_VAR: &str = "ILOVECANDY";

/// Whether the Pac-Man bar is drawn, from the value of [`CANDY_VAR`].
///
/// Unset means yes: this is pacode, the candy is the point. `0`, `false`, `no`
/// and `off` turn it into a plain progress bar for anyone who wants their
/// terminal quiet.
pub fn candy_enabled(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None => true,
        Some(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | ""
        ),
    }
}

/// A plain bar of the same width, for when the candy is switched off.
fn plain_frame(frame: u64, width: usize, glyphs: &Glyphs) -> String {
    if width < 3 {
        return "#".chars().take(width).collect();
    }
    let inner = width - 2;
    let filled = ((frame as usize) % (inner + 1)).min(inner);
    let full = if glyphs.ascii { '#' } else { '█' };
    let empty = if glyphs.ascii { '-' } else { '░' };
    let mut out = String::with_capacity(width);
    out.push('[');
    for i in 0..inner {
        out.push(if i < filled { full } else { empty });
    }
    out.push(']');
    out
}

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
        let tail = format!(
            "{} · {dur_str}",
            phase.label(elapsed_ms, frame, &opts.glyphs)
        );
        let trunc = truncate_to_width(&tail, max_line_width, true);
        return Line::from(Span::styled(trunc, text_style));
    }

    let mut spans = if candy_enabled(std::env::var(CANDY_VAR).ok().as_deref()) {
        pacman_spans(frame, PACMAN_WIDTH, opts)
    } else {
        vec![Span::styled(
            plain_frame(frame, PACMAN_WIDTH, &opts.glyphs),
            opts.theme.dim,
        )]
    };
    let used_w = PACMAN_WIDTH;
    let tail = format!(
        " {} · {dur_str}",
        phase.label(elapsed_ms, frame, &opts.glyphs)
    );
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
    fn candy_is_on_unless_it_is_switched_off() {
        // Unset, and anything that is not a denial, means candy.
        assert!(candy_enabled(None));
        assert!(candy_enabled(Some("true")));
        assert!(candy_enabled(Some("1")));
        assert!(candy_enabled(Some("yes")));
        assert!(candy_enabled(Some("  TRUE  ")));

        for off in ["0", "false", "FALSE", "no", "off", " ", ""] {
            assert!(!candy_enabled(Some(off)), "{off:?} must switch it off");
        }
    }

    #[test]
    fn the_plain_bar_is_the_same_width_as_the_candy_one() {
        let glyphs = Glyphs::new(false);
        for frame in [0, 1, 7, 23, 24, 99] {
            assert_eq!(display_width(&plain_frame(frame, 24, &glyphs)), 24);
            assert_eq!(display_width(&pacman_frame(frame, 24, &glyphs)), 24);
        }
        let ascii = Glyphs::new(true);
        assert_eq!(display_width(&plain_frame(3, 24, &ascii)), 24);
        // It fills as the frame advances, and never past the end.
        let full = plain_frame(1000, 24, &ascii);
        assert!(full.starts_with('[') && full.ends_with(']'));
    }

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

    #[test]
    fn thinking_dots_advance_on_the_activity_line() {
        let opts = RenderOptions::new(80, false);
        let phase = Phase::Thinking;
        let text = |frame: u64| {
            render_activity_line(frame, &opts, &phase, 0, 80)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(text(0).contains("thinking."), "{}", text(0));
        assert!(text(1).contains("thinking.."), "{}", text(1));
        assert!(text(2).contains("thinking…"), "{}", text(2));
        assert!(text(3).contains("thinking."), "{}", text(3));
    }

    #[test]
    fn dimmed_goes_halfway_to_faint_on_rgb_and_uses_dim_elsewhere() {
        let theme = Theme::truecolor();
        let d = dimmed(theme.green, theme.faint);
        let Some(Color::Rgb(r, g, b)) = d.fg else {
            panic!("expected an rgb colour");
        };
        // Midpoint of green (0x96,0xb3,0x5d) and faint (0x43,0x46,0x3f).
        assert_eq!((r, g, b), (0x6c, 0x7c, 0x4e));

        let ansi = Theme::ansi();
        let d = dimmed(ansi.green, ansi.faint);
        assert!(d.add_modifier.contains(Modifier::DIM));

        // pulse() is `base` lit, `dimmed` unlit.
        assert_eq!(pulse(&theme, theme.green, true), theme.green);
        assert_eq!(
            pulse(&theme, theme.green, false),
            dimmed(theme.green, theme.faint)
        );
    }
}
