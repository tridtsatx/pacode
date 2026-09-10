//! What the session is actually doing right now, and how that is worded, coloured
//! and clocked on the activity line (spec §3).
//!
//! Everything here is a pure function of state plus elapsed milliseconds: the draw
//! path must never read the clock itself, or the displayed duration jumps around
//! between redraws instead of ticking once per second.

use ratatui::style::Color;

use pacode_render::Glyphs;

#[cfg(test)]
#[path = "activity_tests.rs"]
mod activity_tests;

/// Thinking wording thresholds, in milliseconds.
const STAGE_1_MS: u64 = 10_000;
const STAGE_2_MS: u64 = 20_000;
const STAGE_3_MS: u64 = 30_000;

/// Thinking reaches full yellow at this age.
const GRADIENT_FULL_MS: u64 = 60_000;

/// What the session is doing. The activity line renders exactly one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The model is reasoning: no tool running, nothing streaming yet.
    Thinking,
    /// The model is producing its answer.
    Responding,
    /// A tool call is executing; the string is the tool's title.
    Tool(String),
    /// The main agent is idle and the session is waiting on a subagent.
    WaitingAgent(String),
    /// The main agent is idle and the session is waiting on background tasks.
    WaitingTask(usize),
}

impl Phase {
    /// The pacman bar means "the model itself is working". Waiting on someone else
    /// is not the model working, so the bar is dropped for those phases.
    pub fn shows_pacman(&self) -> bool {
        match self {
            Self::Thinking | Self::Responding | Self::Tool(_) => true,
            Self::WaitingAgent(_) | Self::WaitingTask(_) => false,
        }
    }

    /// Text shown after the bar. `elapsed_ms` is the age of this phase, `frame`
    /// the shared animation counter that makes the trailing dots move.
    pub fn label(&self, elapsed_ms: u64, frame: u64, glyphs: &Glyphs) -> String {
        match self {
            Self::Thinking => thinking_label(elapsed_ms, frame, glyphs),
            Self::Responding => "responding…".to_string(),
            Self::Tool(title) => title.clone(),
            Self::WaitingAgent(name) => format!("waiting for agent {name}"),
            Self::WaitingTask(n) => {
                if *n == 1 {
                    "waiting for a background task".to_string()
                } else {
                    format!("waiting for {n} background tasks")
                }
            }
        }
    }
}

/// Trailing dots for an in-progress label: `.`, `..`, `…` picked by
/// `frame % 3` (`...` for the last step under ASCII).
pub fn trailing_dots(frame: u64, glyphs: &Glyphs) -> &'static str {
    match frame % 3 {
        0 => ".",
        1 => "..",
        _ => glyphs.ellipsis,
    }
}

/// Thinking wording for the age of the thinking phase; the trailing dots move
/// with the animation frame so the line reads as alive while the model thinks.
pub fn thinking_label(elapsed_ms: u64, frame: u64, glyphs: &Glyphs) -> String {
    let base = if elapsed_ms < STAGE_1_MS {
        "thinking"
    } else if elapsed_ms < STAGE_2_MS {
        "thinking a bit more"
    } else if elapsed_ms < STAGE_3_MS {
        "thinking a lot"
    } else {
        "almost done thinking"
    };
    format!("{base}{}", trailing_dots(frame, glyphs))
}

/// Thinking colour: drifts from `from` (the normal foreground) to `to` (yellow),
/// reaching `to` at 60 s. Continuous, so it does not jump at the wording thresholds.
///
/// Only an RGB pair can be interpolated. When either end is an indexed or named
/// colour — an ANSI terminal, or a theme that uses palette entries — there is
/// nothing to interpolate between, so the colour flips to `to` at the last wording
/// threshold instead. That keeps the escalation visible without inventing colours
/// the terminal does not have.
pub fn thinking_color(elapsed_ms: u64, from: Color, to: Color) -> Color {
    match (from, to) {
        (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) => {
            let t = (elapsed_ms.min(GRADIENT_FULL_MS) as f32) / (GRADIENT_FULL_MS as f32);
            Color::Rgb(
                lerp_channel(r0, r1, t),
                lerp_channel(g0, g1, t),
                lerp_channel(b0, b1, t),
            )
        }
        _ => {
            if elapsed_ms >= STAGE_3_MS {
                to
            } else {
                from
            }
        }
    }
}

fn lerp_channel(a: u8, b: u8, t: f32) -> u8 {
    let a = a as f32;
    let b = b as f32;
    (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
}

/// Whole seconds to display for a phase of age `elapsed_ms`.
///
/// Flooring is what makes the reading stable: a redraw triggered between ticks
/// renders the same number as the tick before it.
pub fn displayed_secs(elapsed_ms: u64) -> u64 {
    elapsed_ms / 1000
}

/// Duration text for the activity line: whole seconds up to a minute, then the
/// shared `m`/`h` formatting. Never sub-second, so the reading cannot flicker.
pub fn format_activity_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        pacode_types::time::format_duration_ms(secs * 1000)
    }
}

/// Milliseconds until the displayed second changes, for a phase of age `elapsed_ms`.
///
/// The event loop re-creates its timers on every iteration, so a plain
/// `sleep(1s)` is cancelled and restarted by every animation frame and never
/// fires. Sleeping only to the next boundary of this clock makes the tick
/// survive that: whatever cancels it, the next arming still lands on the boundary.
pub fn ms_to_next_second(elapsed_ms: u64) -> u64 {
    1000 - (elapsed_ms % 1000)
}
