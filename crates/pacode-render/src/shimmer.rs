//! A light band sweeping across *live* text.
//!
//! The band is what makes a working line read as alive rather than as a label
//! that occasionally changes colour: the same characters brighten as the band
//! crosses them and fall back behind it. Two profiles, both ported from
//! oh-my-pi (`packages/tui/src/theme/shimmer.ts`):
//!
//! - [`ShimmerMode::Classic`] — a cosine bump travelling left to right, with
//!   padding on both ends so the band fades in off-screen instead of popping at
//!   the first cell.
//! - [`ShimmerMode::Kitt`] — a Knight Rider scanner: one bright head
//!   ping-ponging across the text with a quadratic trail behind it and no
//!   leading glow. LEDs do not predict the future.
//!
//! # Position comes from elapsed time, never from a frame counter
//!
//! Every other animation in this client reads its phase off the animation
//! clock (`pacode_tui::backpressure`), so a client that dropped frames skips
//! animation frames instead of running them in slow motion. A sweep has the same
//! requirement and one more: the band must never *jump* a visible cell between
//! two painted frames, or the sweep reads as a flicker rather than as motion.
//! Both fall out of the same rule — the band advances at most one cell per
//! animation tick — so the speed here is derived from the tick rather than
//! fixed. A degraded cadence therefore draws the same band, more sparsely.
//!
//! # Shape of the hot path
//!
//! A live line is re-swept on every animation frame, so the per-character work
//! is the cost that matters:
//!
//! - one pass over the text, no allocation per character;
//! - characters outside the band window take a tier compare and nothing else —
//!   no trigonometry, which is most of the line most of the time;
//! - runs of same-tier characters collapse into one [`Span`], so a 60-cell line
//!   costs a handful of spans rather than sixty.
//!
//! Position is counted in characters, not bytes or UTF-16 units, so a label
//! with an emoji in it bands by the glyphs the user sees.

use ratatui::style::Style;
use ratatui::text::Span;

use pacode_types::config::ShimmerMode;

#[cfg(test)]
#[path = "shimmer_tests.rs"]
mod shimmer_tests;

/// Cells of padding on each end of the classic sweep, so the band enters and
/// leaves off-screen instead of snapping into existence at the first cell.
const CLASSIC_PADDING: f64 = 10.0;

/// Half-width of the classic band, in cells.
const CLASSIC_BAND_HALF_WIDTH: f64 = 6.0;

/// Half-width of the KITT head, in cells.
const KITT_HEAD_HALF: f64 = 0.6;

/// Length of the KITT trail, in cells.
const KITT_TRAIL_LEN: f64 = 7.0;

/// Intensity at or above which a character is at the crest.
const TIER_HIGH: f64 = 0.65;

/// Intensity at or above which a character is on the band's flank.
const TIER_MID: f64 = 0.22;

/// The three styles a character moves through as the band crosses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShimmerPalette {
    /// Off the band entirely.
    pub low: Style,
    /// On the band's flank.
    pub mid: Style,
    /// At the crest.
    pub high: Style,
}

/// Which step of the palette a character is on. Public because
/// [`shimmer_tiers`] is the sweep's test surface: which characters are lit at
/// a given instant is a claim about the band, and a test has to be able to
/// read it without re-deriving the arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Off the band entirely.
    Low,
    /// On the band's flank.
    Mid,
    /// At the crest.
    High,
}

impl Tier {
    fn style(self, palette: &ShimmerPalette) -> Style {
        match self {
            Self::Low => palette.low,
            Self::Mid => palette.mid,
            Self::High => palette.high,
        }
    }
}

/// The animation clock a sweep is drawn at: elapsed time, and the period of
/// whatever is ticking. Only the ratio between the two reaches the band, so a
/// client whose frames cost more than its period paints the same sweep more
/// sparsely instead of drawing a different one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnimClock {
    /// Elapsed animation time.
    pub elapsed_ms: u64,
    /// Period of the driving clock, in milliseconds.
    pub tick_ms: u64,
}

impl AnimClock {
    pub fn new(elapsed_ms: u64, tick_ms: u64) -> Self {
        Self {
            elapsed_ms,
            tick_ms,
        }
    }
}

/// Band travel speed in cells per millisecond: exactly one cell per animation
/// tick, which is the most a painted frame can show without a visible jump.
fn speed_cells_per_ms(tick_ms: u64) -> f64 {
    1.0 / tick_ms.max(1) as f64
}

/// Padding on each end of the classic sweep, capped at half the text.
///
/// The padding exists so the band fades in off-screen rather than popping into
/// existence at the first cell — but it is also dead time, spent with the band
/// nowhere near the text. A live label here is a handful of cells ("running",
/// a tool title), and a fixed ten cells of padding each side would leave a
/// nine-cell label dark most of its cycle: a sweep that reads as a blink. Capping
/// the padding at half the text keeps the fade-in on a long line and keeps a
/// short one lit.
fn classic_padding(total: usize) -> f64 {
    CLASSIC_PADDING.min(total as f64 / 2.0)
}

/// The classic band's position at `elapsed_ms`, in cells from the left edge of
/// the padded run. It travels at a constant speed and wraps, so the sweep
/// period grows with the text rather than the band speeding up on long lines.
fn classic_pos(elapsed_ms: u64, total: usize, tick_ms: u64) -> f64 {
    let period = total as f64 + classic_padding(total) * 2.0;
    (elapsed_ms as f64 * speed_cells_per_ms(tick_ms)) % period
}

/// Cosine bump centred on the band: 1.0 at the crest, 0.0 at the edges.
fn classic_intensity(elapsed_ms: u64, index: usize, total: usize, tick_ms: u64) -> f64 {
    let dist =
        (index as f64 + classic_padding(total) - classic_pos(elapsed_ms, total, tick_ms)).abs();
    if dist >= CLASSIC_BAND_HALF_WIDTH {
        return 0.0;
    }
    0.5 * (1.0 + (std::f64::consts::PI * dist / CLASSIC_BAND_HALF_WIDTH).cos())
}

/// The KITT head's position and direction at `elapsed_ms`.
fn kitt_head(elapsed_ms: u64, total: usize, tick_ms: u64) -> (f64, bool) {
    let range = total as f64 - 1.0;
    let cycle = range * 2.0;
    let sweep = (elapsed_ms as f64 * speed_cells_per_ms(tick_ms)) % cycle;
    let going_right = sweep < range;
    (if going_right { sweep } else { cycle - sweep }, going_right)
}

/// Bright head, quadratic trail behind it, nothing ahead of it.
///
/// A line one cell wide has no range to travel over: the only cell *is* the
/// head, always. That guard is also what keeps the cycle above non-zero — a
/// modulo by zero makes every comparison below NaN, and a NaN tier paints the
/// whole line dark.
fn kitt_intensity(elapsed_ms: u64, index: usize, total: usize, tick_ms: u64) -> f64 {
    if total <= 1 {
        return 1.0;
    }
    let (head, going_right) = kitt_head(elapsed_ms, total, tick_ms);
    let delta = index as f64 - head;
    if delta.abs() <= KITT_HEAD_HALF {
        return 1.0;
    }
    let behind = if going_right { -delta } else { delta };
    if behind <= KITT_HEAD_HALF {
        return 0.0;
    }
    let travelled = (behind - KITT_HEAD_HALF) / KITT_TRAIL_LEN;
    if travelled >= 1.0 {
        return 0.0;
    }
    let remaining = 1.0 - travelled;
    remaining * remaining
}

fn tier_of(intensity: f64) -> Tier {
    if intensity >= TIER_HIGH {
        Tier::High
    } else if intensity >= TIER_MID {
        Tier::Mid
    } else {
        Tier::Low
    }
}

/// Character indices outside which the intensity is zero for `mode` right now.
/// Widening this window is always safe — the intensity call still runs inside
/// it and reports zero — so it is derived from the same arithmetic as the
/// intensity rather than from a second, separately-tuned bound.
fn active_band(mode: ShimmerMode, elapsed_ms: u64, total: usize, tick_ms: u64) -> (f64, f64) {
    match mode {
        ShimmerMode::Classic => {
            let pos = classic_pos(elapsed_ms, total, tick_ms);
            let padding = classic_padding(total);
            (
                pos - padding - CLASSIC_BAND_HALF_WIDTH,
                pos - padding + CLASSIC_BAND_HALF_WIDTH,
            )
        }
        ShimmerMode::Kitt => {
            if total <= 1 {
                return (0.0, total as f64);
            }
            let (head, going_right) = kitt_head(elapsed_ms, total, tick_ms);
            if going_right {
                (
                    head - KITT_HEAD_HALF - KITT_TRAIL_LEN,
                    head + KITT_HEAD_HALF,
                )
            } else {
                (
                    head - KITT_HEAD_HALF,
                    head + KITT_HEAD_HALF + KITT_TRAIL_LEN,
                )
            }
        }
        ShimmerMode::Off => (f64::INFINITY, f64::NEG_INFINITY),
    }
}

fn intensity(mode: ShimmerMode, elapsed_ms: u64, index: usize, total: usize, tick_ms: u64) -> f64 {
    match mode {
        ShimmerMode::Classic => classic_intensity(elapsed_ms, index, total, tick_ms),
        ShimmerMode::Kitt => kitt_intensity(elapsed_ms, index, total, tick_ms),
        ShimmerMode::Off => 0.0,
    }
}

/// Number of characters in `text`, counting a surrogate-free Rust `char` as
/// one — which is what the band moves over, and what the eye reads as a cell.
fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// `text` swept by the band, as spans of same-tier runs.
///
/// `elapsed_ms` is elapsed animation time and `tick_ms` the period of the clock
/// driving it; the two only ever appear as their ratio, so any monotonic clock
/// works and a degraded cadence moves the band more slowly rather than in
/// visible jumps.
///
/// With [`ShimmerMode::Off`], or no text, the result is a single span in the
/// palette's mid style — the line stays legible and the per-character walk
/// never runs.
pub fn shimmer_spans(
    text: &str,
    mode: ShimmerMode,
    elapsed_ms: u64,
    tick_ms: u64,
    palette: ShimmerPalette,
) -> Vec<Span<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    if !mode.enabled() {
        return vec![Span::styled(text.to_string(), palette.mid)];
    }

    let total = char_count(text);
    let (band_lo, band_hi) = active_band(mode, elapsed_ms, total, tick_ms);

    // Coalescing is what makes this cheap: a 60-cell line emits a handful of
    // spans, not sixty. A run is flushed when the *tier* changes — comparing
    // byte offsets instead would flush on every character, since offsets only
    // ever grow.
    let mut spans = Vec::with_capacity(8);
    let mut run_tier = Tier::Low;
    let mut run_start = 0;
    let mut run_end = 0;
    for (index, (offset, ch)) in text.char_indices().enumerate() {
        let in_band = (index as f64) >= band_lo && (index as f64) <= band_hi;
        let tier = if in_band {
            tier_of(intensity(mode, elapsed_ms, index, total, tick_ms))
        } else {
            Tier::Low
        };
        if tier != run_tier && run_end > run_start {
            spans.push(Span::styled(
                text[run_start..run_end].to_string(),
                run_tier.style(&palette),
            ));
            run_start = offset;
        }
        run_tier = tier;
        run_end = offset + ch.len_utf8();
    }
    if run_end > run_start {
        spans.push(Span::styled(
            text[run_start..run_end].to_string(),
            run_tier.style(&palette),
        ));
    }
    spans
}

/// Tiers one label would be drawn in at `elapsed_ms`, character by character.
/// The sweep's own test surface: a label's band position is a claim about which
/// characters are lit, and this is that claim without the styling.
pub fn shimmer_tiers(text: &str, mode: ShimmerMode, elapsed_ms: u64, tick_ms: u64) -> Vec<Tier> {
    let total = char_count(text);
    if !mode.enabled() || total == 0 {
        return vec![Tier::Mid; total];
    }
    let (band_lo, band_hi) = active_band(mode, elapsed_ms, total, tick_ms);
    (0..total)
        .map(|index| {
            if (index as f64) >= band_lo && (index as f64) <= band_hi {
                tier_of(intensity(mode, elapsed_ms, index, total, tick_ms))
            } else {
                Tier::Low
            }
        })
        .collect()
}
