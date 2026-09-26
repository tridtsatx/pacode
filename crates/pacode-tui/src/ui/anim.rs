//! Pacman activity animation (spec §3, Arch ILoveCandy style).
//!
//! While `turn_active` (main agent thinking or running a tool), an animated status
//! line is rendered as the last line of the dialog area:
//! `C` walking right over a fixed candy track, chomping `C`/`c` as it goes.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::state::activity::{Phase, displayed_secs, format_activity_secs, thinking_color};
use pacode_render::{
    Glyphs, RenderOptions, ShimmerPalette, Theme, display_width, shimmer_spans, truncate_to_width,
};

/// Display width of the activity bar, including its brackets.
const PACMAN_WIDTH: usize = 24;

/// Frames between pacman's steps, tuned for the 66 ms animation tick: one
/// cell every ~132 ms ≈ 7.6 cells/s, so he crosses the 22-cell inner track
/// in ~2.9 s — a walk, not the old two-cell teleport.
const PACMAN_MOVE_DIVIDER: u64 = 2;

/// Frames between mouth states, tuned for the 66 ms animation tick: the
/// `C`/`c` toggle lands every ~200 ms, so he visibly chomps (~5 Hz) instead
/// of flickering on every redraw.
const PACMAN_MOUTH_DIVIDER: u64 = 3;

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

/// The env lookup runs once per process: reading `std::env` on every drawn
/// frame showed up on the hot path, and a toggle mid-session was never a
/// feature anyone asked for.
static CANDY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// A plain bar of the same width, for when the candy is switched off. It
/// fills on the pacman's own step divider, so the quiet bar walks at the
/// same speed as the candy one.
fn plain_frame(frame: u64, width: usize, glyphs: &Glyphs) -> String {
    if width < 3 {
        return "#".chars().take(width).collect();
    }
    let inner = width - 2;
    let filled = ((frame / PACMAN_MOVE_DIVIDER) as usize) % (inner + 1);
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
/// Format: `[C • • • • • ]` -> `[  c • • • • ]`. The candies sit on fixed
/// cells (every other inner cell) and never shift — pacman eats them as he
/// walks through, leaving a blank trail. The wrap from the last cell back
/// to the first is one more step on the loop, with the candy track restored.
/// Width is the total display width including `[` and `]`.
pub fn pacman_frame(frame: u64, width: usize, glyphs: &Glyphs) -> String {
    if width < 3 {
        return "C".chars().take(width).collect();
    }

    let inner_w = width - 2;
    let candy = if glyphs.ascii { 'o' } else { '•' };
    let mouth = if (frame / PACMAN_MOUTH_DIVIDER).is_multiple_of(2) {
        'C'
    } else {
        'c'
    };

    // One cell per step — the two-cell hop teleported him past the gaps.
    let pos = ((frame / PACMAN_MOVE_DIVIDER) as usize) % inner_w;

    let mut out = String::with_capacity(width + 8);
    out.push('[');
    for i in 0..inner_w {
        if i == pos {
            out.push(mouth);
        } else if i < pos {
            // Behind him: the candy that stood here is eaten.
            out.push(' ');
        } else if i.is_multiple_of(2) {
            // Ahead of him: candies live on fixed cells, so the track never
            // slides under his feet.
            out.push(candy);
        } else {
            out.push(' ');
        }
    }
    out.push(']');
    out
}

/// `{lead}{label}` on the left, `{right}` flush against `width` on the
/// right, at least one cell between them. The counters keep a fixed right
/// edge, so a wording change ("thinking" → "thinking a bit more") never
/// moves them (§9.2).
fn spread_tail(lead: &str, label: &str, right: &str, width: usize) -> String {
    let lead_w = display_width(lead);
    let right_w = display_width(right);
    if width <= lead_w + right_w {
        // No room for the label at all — keep the counters, they are the
        // stable part.
        return truncate_to_width(&format!("{lead}{right}"), width, true);
    }
    let budget = width - lead_w - right_w - 1;
    let label = truncate_to_width(label, budget, true);
    let fill = width - lead_w - display_width(&label) - right_w;
    format!("{lead}{label}{}{right}", " ".repeat(fill))
}

/// Renders the activity line for `phase`.
///
/// A phase exists only while the main agent's own turn is running, so the
/// pacman bar always shows. The duration is whole seconds, so a redraw between
/// ticks shows the same value as the tick before it.
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
    };
    let label = phase.label(elapsed_ms, frame, &opts.glyphs);

    let mut spans =
        if *CANDY.get_or_init(|| candy_enabled(std::env::var(CANDY_VAR).ok().as_deref())) {
            pacman_spans(frame, PACMAN_WIDTH, opts)
        } else {
            vec![Span::styled(
                plain_frame(frame, PACMAN_WIDTH, &opts.glyphs),
                opts.theme.dim,
            )]
        };
    let avail = max_line_width.saturating_sub(PACMAN_WIDTH);
    // The tail is the one line on screen that only ever exists while the agent
    // is working, so it is where a sweep earns its keep: a band crossing the
    // wording reads as work in progress, where a colour that changes once a
    // second reads as a repaint. `spread_tail` lays the counters out first, so
    // a band lighting a different number of cells each frame cannot move them.
    let tail = spread_tail(" ", &label, &dur_str, avail);
    spans.extend(sweep_spans(tail, text_style, opts));
    Line::from(spans)
}

/// `text` under a moving band, in the styles the band is passing through.
///
/// The text is taken by value so the switched-off path can hand its `String`
/// straight to the span: copying it would add a heap allocation and a full
/// memcpy of the line to every animation frame for a sweep that is not drawn.
/// That path returns the caller's own style, so the line is not merely the same
/// on screen — it costs exactly what it cost before sweeps existed.
///
/// The palette is built around the caller's style rather than around the theme:
/// the low tier is that colour dimmed toward `faint` and the crest is the
/// accent, so a line keeps its identity (a thinking line keeps its thinking
/// colour) instead of turning into generic theme text.
pub(crate) fn sweep_spans(text: String, base: Style, opts: &RenderOptions) -> Vec<Span<'static>> {
    if !opts.shimmer.enabled() {
        return vec![Span::styled(text, base)];
    }
    let palette = ShimmerPalette {
        low: dimmed(base, opts.theme.faint),
        mid: base,
        high: opts.theme.accent.add_modifier(Modifier::BOLD),
    };
    shimmer_spans(
        &text,
        opts.shimmer,
        opts.anim.elapsed_ms,
        opts.anim.tick_ms,
        palette,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_render::display_width;
    use pacode_types::config::ShimmerMode;

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
    fn test_pacman_frame_ascii() {
        let glyphs = Glyphs::new(true);
        // Step 0 (frames 0..2): pacman on cell 0, mouth still wide —
        // PACMAN_MOUTH_DIVIDER is 3, so the first `c` appears at frame 3.
        let f0 = pacman_frame(0, 14, &glyphs);
        assert_eq!(f0, "[C o o o o o ]");

        // Step 1 (frames 2..4): one cell right, mouth to the candy at cell 2.
        let f2 = pacman_frame(2, 14, &glyphs);
        assert_eq!(f2, "[ Co o o o o ]");

        // Same cell, but the mouth divider has flipped to the `c` half.
        let f3 = pacman_frame(3, 14, &glyphs);
        assert_eq!(f3, "[ co o o o o ]");

        // Step 2 (frames 4..6): one more cell, the candy at cell 2 eaten.
        let f4 = pacman_frame(4, 14, &glyphs);
        assert_eq!(f4, "[  c o o o o ]");
    }

    #[test]
    fn test_pacman_frame_unicode() {
        let glyphs = Glyphs::new(false);
        let f0 = pacman_frame(0, 14, &glyphs);
        assert_eq!(f0, "[C • • • • • ]");

        let f3 = pacman_frame(3, 14, &glyphs);
        assert_eq!(f3, "[ c• • • • • ]");
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
    fn pacman_never_jumps_more_than_one_cell() {
        // The track is a loop: position is a cell index into `inner`, so a
        // step is `(next - prev) mod inner` — the wrap from the last cell to
        // the first measures as +1, not as a cross-screen teleport.
        for ascii in [true, false] {
            let glyphs = Glyphs::new(ascii);
            let inner = PACMAN_WIDTH - 2;
            let pos = |frame: u64| {
                pacman_frame(frame, PACMAN_WIDTH, &glyphs)
                    .chars()
                    .position(|ch| ch == 'C' || ch == 'c')
                    .expect("pacman is always on the track")
                    - 1
            };
            let mut prev = pos(0);
            // Two full position loops, so the wrap is exercised both ways.
            for frame in 1..(2 * PACMAN_MOVE_DIVIDER * inner as u64) {
                let cur = pos(frame);
                let step = (cur + inner - prev) % inner;
                assert!(step <= 1, "frame {frame}: jumped {step} cells");
                prev = cur;
            }
        }
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
        // The dots step once per DOT_DIVIDER frames — at the 66 ms tick a
        // dot lands every ~0.5 s instead of strobing per frame.
        let d = crate::state::activity::DOT_DIVIDER;
        assert!(text(0).contains("thinking.  "), "{}", text(0));
        assert!(text(d).contains("thinking.. "), "{}", text(d));
        assert!(text(2 * d).contains("thinking..."), "{}", text(2 * d));
        assert!(text(3 * d).contains("thinking.  "), "{}", text(3 * d));
        // And it holds still between steps — that is the point of dividing.
        assert!(text(1).contains("thinking.  "), "{}", text(1));
    }

    #[test]
    fn the_duration_is_pinned_to_the_right_edge() {
        // §9.2: the counters never move — a wording change on the left must
        // not shift the duration at the right edge.
        let opts = RenderOptions::new(80, false);
        let phase = Phase::Thinking;
        let text = |elapsed: u64, frame: u64| {
            render_activity_line(frame, &opts, &phase, elapsed, 80)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        for (elapsed, dur) in [
            (0_u64, "0s"),
            (15_000, "15s"),
            (25_000, "25s"),
            (45_000, "45s"),
        ] {
            for frame in [0_u64, 1, 2] {
                let line = text(elapsed, frame);
                assert_eq!(display_width(&line), 80, "{line:?}");
                assert!(line.ends_with(dur), "{line:?}");
            }
        }

        // A tool phase reads the same bar — every phase is the main agent's
        // own work now, so the pacman always shows.
        let tool = Phase::Tool("bash cargo test".to_string());
        let line = render_activity_line(0, &opts, &tool, 5_000, 80)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(display_width(&line), 80, "{line:?}");
        assert!(line.contains("bash cargo test"), "{line:?}");
        assert!(line.ends_with("5s"), "{line:?}");
    }

    #[test]
    fn a_narrow_line_drops_the_label_before_the_duration() {
        let opts = RenderOptions::new(40, false);
        let phase = Phase::Tool("a very long tool title that will not fit".to_string());
        let line = render_activity_line(0, &opts, &phase, 3_000, 32)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        // 24 cells of bar + the tail squeezed to the remaining 8.
        assert_eq!(display_width(&line), 32, "{line:?}");
        assert!(line.ends_with("3s"), "{line:?}");
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

    /// The activity line's tail under a band: its text, its span styles, and
    /// the cell offset where the crest starts (`None` while the band is off
    /// the text entirely).
    fn swept_tail(elapsed_ms: u64, tick_ms: u64) -> (String, Vec<Style>, Option<usize>) {
        let opts = RenderOptions::new(80, false)
            .shimmer(ShimmerMode::Classic)
            .anim(pacode_render::AnimClock::new(elapsed_ms, tick_ms));
        let crest = opts.theme.accent.add_modifier(Modifier::BOLD);
        let line = render_activity_line(0, &opts, &Phase::Thinking, 0, 80);
        let text = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        let styles = line.spans.iter().map(|s| s.style).collect::<Vec<_>>();
        let crest_at = line
            .spans
            .iter()
            .scan(0usize, |cell, s| {
                let start = *cell;
                *cell += display_width(&s.content);
                Some((start, s.style == crest))
            })
            .find(|(_, is_crest)| *is_crest)
            .map(|(start, _)| start);
        (text, styles, crest_at)
    }

    #[test]
    fn a_disabled_sweep_leaves_the_activity_line_exactly_as_it_was() {
        // One span, in the caller's own style: no band, no extra runs, no
        // per-character walk. That is the whole promise of `ui.shimmer = off`.
        let opts = RenderOptions::new(80, false).shimmer(ShimmerMode::Off);
        let line = render_activity_line(0, &opts, &Phase::Thinking, 0, 80);
        let tail = line
            .spans
            .last()
            .expect("the bar and the tail are both drawn");
        assert!(
            tail.content.starts_with(" thinking.  "),
            "{:?}",
            tail.content
        );
        assert!(tail.content.ends_with("0s"), "{:?}", tail.content);
        // The tail still fills the line, so the duration stays pinned to the
        // right edge and a wording change still cannot move it.
        assert_eq!(display_width(&tail.content), 80 - PACMAN_WIDTH);
        assert!(!tail.style.add_modifier.contains(Modifier::DIM));
        assert!(!tail.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_swept_activity_line_keeps_its_text_and_moves_its_colours() {
        // The user-visible contract of a sweep: identical wording on every
        // frame, different colouring. Shuffled text or a frozen band fail.
        let (first_text, first_styles, _) = swept_tail(0, 66);
        let mut moved = false;
        let mut crest_seen = false;
        let mut previous = first_styles;
        for step in 1..200u64 {
            let (text, styles, crest) = swept_tail(step * 66, 66);
            assert_eq!(text, first_text, "the wording changed at tick {step}");
            moved |= styles != previous;
            crest_seen |= crest.is_some();
            previous = styles;
        }
        assert!(moved, "the band never changed a single cell in 200 frames");
        assert!(crest_seen, "the crest never reached the line");
    }

    #[test]
    fn the_band_never_jumps_a_cell_at_any_cadence() {
        // What the period-in-force buys: a degraded cadence paints the same
        // band more sparsely. A band still moving a cell per *nominal* tick
        // would jump nine cells per painted frame under the ×9 ceiling, which
        // is the stutter backpressure exists to avoid. Frames with the band
        // off the text carry no position, and a frame where the band is still
        // entering pins the crest against the first cell — so the invariant is
        // "it travels, at most one cell per painted frame", not "+1 every frame".
        for tick_ms in [66u64, 100, 240, 66 * 9] {
            let mut previous: Option<usize> = None;
            let mut travel = 0i64;
            for step in 0..400u64 {
                let crest = swept_tail(step * tick_ms, tick_ms).2;
                let moved = match (crest, previous) {
                    (Some(now), Some(before)) => {
                        let cells = now as i64 - before as i64;
                        assert!(
                            (0..=1).contains(&cells),
                            "at a {tick_ms}ms period the band moved {cells} cells at tick {step}"
                        );
                        cells
                    }
                    _ => 0,
                };
                travel += moved;
                previous = crest;
            }
            assert!(
                travel >= 40,
                "at a {tick_ms}ms period the band only travelled {travel} cells"
            );
        }
    }
}
