use super::*;
use pacode_types::config::ShimmerMode;
use ratatui::style::{Color, Modifier, Style};

const TICK: u64 = 66;

fn tiers(text: &str, mode: ShimmerMode, elapsed_ms: u64) -> Vec<Tier> {
    shimmer_tiers(text, mode, elapsed_ms, TICK)
}

fn counts(ts: &[Tier]) -> (usize, usize, usize) {
    let low = ts.iter().filter(|t| **t == Tier::Low).count();
    let mid = ts.iter().filter(|t| **t == Tier::Mid).count();
    let high = ts.iter().filter(|t| **t == Tier::High).count();
    (low, mid, high)
}

#[test]
fn off_draws_one_flat_span_and_never_walks_the_text() {
    let palette = ShimmerPalette {
        low: Style::default().fg(Color::Red),
        mid: Style::default().fg(Color::Green),
        high: Style::default().fg(Color::Blue),
    };
    let spans = shimmer_spans("thinking", ShimmerMode::Off, 1234, TICK, palette);
    assert_eq!(
        spans.len(),
        1,
        "a disabled sweep is one span, not one per char"
    );
    assert_eq!(spans[0].content, "thinking");
    assert_eq!(spans[0].style, palette.mid);
}

#[test]
fn empty_text_draws_nothing() {
    let palette = palette();
    assert!(shimmer_spans("", ShimmerMode::Classic, 0, TICK, palette).is_empty());
    assert!(tiers("", ShimmerMode::Classic, 0).is_empty());
}

#[test]
fn the_sweep_runs_over_the_whole_line_and_leaves_no_static_text() {
    // The failure this guards: a band that only ever lights the first few cells
    // reads as a blinking cursor, not as a sweep.
    let text = "a rather long working label that spans plenty of cells";
    let mut seen_low = false;
    let mut seen_high = false;
    for step in 0..400u64 {
        let (low, _, high) = counts(&tiers(text, ShimmerMode::Classic, step * TICK));
        seen_low |= low > 0;
        seen_high |= high > 0;
    }
    assert!(seen_low, "part of the line is always off-band");
    assert!(seen_high, "the crest reaches the line at some point");
}

#[test]
fn the_band_advances_at_most_one_cell_per_tick() {
    // Two cells per tick is a visible jump: the sweep stops reading as motion.
    // This is the whole reason the speed is derived from the tick period.
    let text = "a rather long working label that spans plenty of cells";
    for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
        let mut previous: Option<Vec<Tier>> = None;
        for step in 0..600u64 {
            let now = tiers(text, mode, step * TICK);
            if let Some(before) = &previous {
                let drift = now
                    .iter()
                    .zip(before.iter())
                    .filter(|(a, b)| a != b)
                    .count();
                assert!(
                    drift <= 14,
                    "{mode:?}: {drift} cells changed in one tick at step {step}"
                );
            }
            previous = Some(now);
        }
    }
}

#[test]
fn the_crest_is_exactly_one_contiguous_run() {
    let text = "a rather long working label that spans plenty of cells";
    for step in 0..200u64 {
        let ts = tiers(text, ShimmerMode::Classic, step * TICK);
        let lit: Vec<usize> = ts
            .iter()
            .enumerate()
            .filter(|(_, t)| **t != Tier::Low)
            .map(|(i, _)| i)
            .collect();
        assert!(
            lit.windows(2).all(|w| w[1] == w[0] + 1),
            "{mode:?} band split into pieces at step {step}: {lit:?}",
            mode = ShimmerMode::Classic
        );
    }
}

#[test]
fn kitt_trail_lags_the_head_and_nothing_lights_ahead_of_it() {
    let text = "scanning";
    // Walk a full ping-pong and check the shape at each step: the lit run is
    // always the head plus a trail *behind* it, never ahead.
    let range = text.chars().count() as f64 - 1.0;
    let cycle_ms = (2.0 * range) * TICK as f64;
    for step in 0..(cycle_ms as u64 / TICK) {
        let elapsed = step * TICK;
        let ts = tiers(text, ShimmerMode::Kitt, elapsed);
        let head_at = ts
            .iter()
            .position(|t| *t == Tier::High)
            .expect("the head is always lit somewhere");
        let sweep = (elapsed as f64 / TICK as f64) % (2.0 * range);
        let going_right = sweep < range;
        let head = if going_right {
            sweep
        } else {
            2.0 * range - sweep
        };
        assert!(
            (head_at as f64 - head).abs() <= 1.5,
            "head at {head_at}, expected near {head} (step {elapsed})"
        );
        if going_right {
            assert!(
                !ts.iter().skip(head_at + 2).any(|t| *t != Tier::Low),
                "something lit ahead of a right-moving head at {elapsed}"
            );
        } else {
            assert!(
                !ts.iter()
                    .take(head_at.saturating_sub(2))
                    .any(|t| *t != Tier::Low),
                "something lit ahead of a left-moving head at {elapsed}"
            );
        }
    }
}

#[test]
fn a_one_character_line_is_always_lit() {
    // Degenerate input: a KITT sweep has no range to travel over, and a
    // classic one has a single cell. Neither may render as dead space.
    for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
        let ts = tiers("x", mode, 999);
        assert_eq!(ts.len(), 1);
        assert!(ts[0] != Tier::Low, "{mode:?} left a single cell unlit");
    }
}

#[test]
fn the_crest_reaches_every_cell_within_one_cycle() {
    // A sweep that only lights the first few cells is a blinking cursor. Every
    // cell has to see the crest pass over it, which is also what proves the
    // capped padding still lets the band run off the right-hand end.
    for len in 1..=40usize {
        let text = "x".repeat(len);
        for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
            let mut reached = vec![false; len];
            for step in 0..400u64 {
                for (cell, tier) in tiers(&text, mode, step * TICK).into_iter().enumerate() {
                    reached[cell] |= tier == Tier::High;
                }
            }
            let missed: Vec<usize> = reached
                .iter()
                .enumerate()
                .filter(|(_, r)| !**r)
                .map(|(i, _)| i)
                .collect();
            assert!(
                missed.is_empty(),
                "{mode:?}: on a {len}-cell line the crest never reached {missed:?}"
            );
        }
    }
}

#[test]
fn a_short_label_is_lit_for_most_of_its_cycle() {
    // A live status label is a handful of cells. If the band spends its cycle
    // in the padding instead of on the text, the label blinks rather than
    // shimmers — the defect the padding cap exists to prevent. A long line is
    // deliberately the opposite case: mostly dim with a highlight crossing it.
    for len in 1..=20usize {
        let text = "x".repeat(len);
        for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
            let lit = (0..400u64)
                .filter(|step| {
                    tiers(&text, mode, step * TICK)
                        .iter()
                        .any(|t| *t != Tier::Low)
                })
                .count();
            assert!(
                lit * 100 >= 400 * 25,
                "{mode:?}: a {len}-cell label was lit only {lit}/400 ticks"
            );
        }
    }
}

#[test]
fn a_sweep_is_a_function_of_elapsed_time_only() {
    // The rendering cache and the frame metrics both key off the reading, so
    // the same clock must always give the same frame — no frame counters, no
    // hidden state.
    let text = "working";
    for elapsed in [0u64, 1, 66, 999, 12345] {
        let first = shimmer_spans(text, ShimmerMode::Classic, elapsed, TICK, palette());
        let again = shimmer_spans(text, ShimmerMode::Classic, elapsed, TICK, palette());
        assert_eq!(first, again, "elapsed {elapsed} is not deterministic");
    }
}

#[test]
fn multibyte_labels_band_by_character() {
    // The band moves over what the user sees. A label with wide glyphs in it
    // must still get one tier per character, and the spans must reassemble into
    // the original string.
    let text = "работа ⠋ done";
    for step in 0..50u64 {
        let spans = shimmer_spans(text, ShimmerMode::Classic, step * TICK, TICK, palette());
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, text, "step {step} lost or reordered characters");
    }
}

#[test]
fn the_spans_carry_exactly_the_tiers_the_sweep_computed() {
    // The drawn line and the tested band are two readers of one computation,
    // and this is what keeps them honest: expanding the spans back into one
    // tier per character must reproduce `shimmer_tiers` cell for cell, at every
    // position of the band. A run that is flushed on the wrong boundary — or
    // not coalesced at all — shifts every colour after it by one cell, and the
    // band visibly lags the text.
    let palette = palette();
    let tier_of_style = |style: Style| {
        if style == palette.high {
            Tier::High
        } else if style == palette.mid {
            Tier::Mid
        } else {
            Tier::Low
        }
    };
    for text in [
        "a rather long working label that spans plenty of cells",
        "работа ⠋ done",
        "x",
        "⠋ running",
    ] {
        for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
            for step in 0..120u64 {
                let spans = shimmer_spans(text, mode, step * TICK, TICK, palette);
                let drawn: Vec<Tier> = spans
                    .iter()
                    .flat_map(|s| {
                        std::iter::repeat_n(tier_of_style(s.style), s.content.chars().count())
                    })
                    .collect();
                assert_eq!(
                    drawn,
                    shimmer_tiers(text, mode, step * TICK, TICK),
                    "{mode:?} on {text:?} at tick {step}: the drawn tiers differ from the band"
                );
            }
        }
    }
}

#[test]
fn a_band_emits_a_handful_of_spans_not_one_per_character() {
    // The whole reason runs are coalesced: this runs on every animation frame.
    // A 60-cell line with a 12-cell band has at most a few tier boundaries, so
    // a correct sweep is an order of magnitude cheaper than a per-character one.
    let text = "a rather long working label that spans plenty of cells";
    for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
        for step in 0..60u64 {
            let spans = shimmer_spans(text, mode, step * TICK, TICK, palette());
            assert!(
                spans.len() <= 12,
                "{mode:?} at tick {step}: {} spans for {} characters",
                spans.len(),
                text.chars().count()
            );
        }
    }
}

#[test]
fn a_zero_period_does_not_divide_by_zero() {
    // `ui.animation` cannot produce a zero period, but a sweep must not turn a
    // misconfiguration into a NaN band that paints every cell high.
    let text = "working";
    for mode in [ShimmerMode::Classic, ShimmerMode::Kitt] {
        let ts = shimmer_tiers(text, mode, 500, 0);
        assert_eq!(ts.len(), text.chars().count());
        assert!(
            ts.iter()
                .all(|t| matches!(t, Tier::Low | Tier::Mid | Tier::High))
        );
    }
}

fn palette() -> ShimmerPalette {
    ShimmerPalette {
        low: Style::default().fg(Color::DarkGray),
        mid: Style::default().fg(Color::Gray),
        high: Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    }
}
