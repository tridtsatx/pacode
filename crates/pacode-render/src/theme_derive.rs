//! Theme roles a palette may leave out: derived from the roles it does
//! declare, in sRGB, and only when every input is RGB. A terminal limited to
//! named colours gets no band at all: a named background is either the
//! terminal's own background (invisible) or a loud block.

use ratatui::style::{Color, Style};

use crate::theme::Theme;

/// How far `surface` moves from `selected_bg` towards `faint`.
const SURFACE_MIX: f32 = 0.35;
/// How far `border` moves from `faint` towards `dim`.
const BORDER_MIX: f32 = 0.4;
/// How far `panel` moves from `selected_bg` away from the text (towards black
/// on a dark palette, white on a light one): a selected row stays brighter
/// (darker) than the panel it is drawn on.
const PANEL_MIX: f32 = 0.5;
/// The neutral tool card sits halfway between `panel` and `selected_bg`.
const CARD_MIX: f32 = 0.5;
/// How far each tool card is tinted towards its status tone. Success is the
/// common case and stays calmest; an error is the one to notice.
const TOOL_PENDING_TINT: f32 = 0.10;
const TOOL_SUCCESS_TINT: f32 = 0.08;
const TOOL_ERROR_TINT: f32 = 0.15;
/// How far the two status-line shades move from `panel` towards `faint`.
const STATUS_MIX: f32 = 0.25;
const STATUS_ALT_MIX: f32 = 0.45;

impl Theme {
    /// Fills `panel`, the tool cards and the status shades from the roles
    /// already set. `explicit(key)` is the palette's own value for a token,
    /// which wins over the derivation; an explicit `panel` also feeds the
    /// tokens built on it.
    pub(crate) fn derive_backgrounds(
        &mut self,
        light: bool,
        explicit: impl Fn(&str) -> Option<Color>,
    ) {
        let pick =
            |key: &str, derived: Style| explicit(key).map_or(derived, |c| Style::default().bg(c));
        let away = if light { WHITE } else { BLACK };
        self.panel = pick("panel", blend(self.selected_bg.bg, Some(away), PANEL_MIX));
        let card = blend(self.panel.bg, self.selected_bg.bg, CARD_MIX).bg;
        self.tool_pending_bg = pick(
            "tool_pending_bg",
            blend(card, self.accent.fg, TOOL_PENDING_TINT),
        );
        self.tool_success_bg = pick(
            "tool_success_bg",
            blend(card, self.green.fg, TOOL_SUCCESS_TINT),
        );
        self.tool_error_bg = pick("tool_error_bg", blend(card, self.red.fg, TOOL_ERROR_TINT));
        self.status_bg = pick("status_bg", blend(self.panel.bg, self.faint.fg, STATUS_MIX));
        self.status_bg_alt = pick(
            "status_bg_alt",
            blend(self.panel.bg, self.faint.fg, STATUS_ALT_MIX),
        );
    }
}

const BLACK: Color = Color::Rgb(0, 0, 0);
const WHITE: Color = Color::Rgb(0xff, 0xff, 0xff);

/// `selected_bg` pulled towards `faint`, so a selection painted over the
/// band stays visible.
pub(crate) fn derive_surface(selected_bg: Style, faint: Style) -> Style {
    blend(selected_bg.bg, faint.fg, SURFACE_MIX)
}

/// Between `faint` and `dim` when both are RGB; `faint` itself otherwise.
pub(crate) fn derive_border(faint: Style, dim: Style) -> Style {
    match (faint.fg, dim.fg) {
        (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) => {
            Style::default().fg(mix((r1, g1, b1), (r2, g2, b2), BORDER_MIX))
        }
        _ => faint,
    }
}

/// A background `t` of the way from `a` to `b` when both are RGB; no
/// background otherwise.
fn blend(a: Option<Color>, b: Option<Color>, t: f32) -> Style {
    match (a, b) {
        (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) => {
            Style::default().bg(mix((r1, g1, b1), (r2, g2, b2), t))
        }
        _ => Style::default(),
    }
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> Color {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// WCAG 2 relative luminance of an sRGB colour.
pub(crate) fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

#[cfg(test)]
#[path = "theme_derive_tests.rs"]
mod theme_derive_tests;
