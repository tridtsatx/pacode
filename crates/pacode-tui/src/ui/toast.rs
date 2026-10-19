//! Toasts (spec §7): a plain strip of up to `TOAST_STRIP_MAX` lines directly
//! above the input separator, 6 s TTL: `✓ cargo build finished · 3m02s`.
//!
//! The strip has no box and no slide-in — the layout gives it dedicated rows
//! carved from the dialog's bottom, so no `Clear` is needed either. The only
//! animation left is the fade to `theme.faint` over the last `TOAST_EXIT_MS`
//! of a toast's life, riding the anim tick.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::state::ToastLevel;

use crate::layout::TOAST_STRIP_MAX;
use crate::state::{AppState, TOAST_EXIT_MS, TOAST_TTL_SECS, Toast};

/// Whether a toast of `age_ms` is inside its fade-out window.
pub fn is_fading(age_ms: u64) -> bool {
    (TOAST_TTL_SECS * 1000).saturating_sub(age_ms) < TOAST_EXIT_MS
}

/// The toasts the strip shows: unexpired, capped at `TOAST_STRIP_MAX`, oldest
/// first so the newest lands on the row closest to the input. `ui::draw` sizes
/// `ScreenLayout::toast` from this, so it is the strip's single source of truth.
pub fn live_toasts(state: &AppState, now: Instant) -> Vec<&Toast> {
    let mut out: Vec<&Toast> = state
        .toasts
        .iter()
        .rev()
        .filter(|t| now.saturating_duration_since(t.shown_at).as_secs() < TOAST_TTL_SECS)
        // A toast linked to an input token dies with it (deleted/submitted).
        .filter(|t| {
            t.token_label
                .as_ref()
                .is_none_or(|label| state.input.text.contains(label.as_str()))
        })
        .take(TOAST_STRIP_MAX as usize)
        .collect();
    out.reverse();
    out
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let now = Instant::now();
    let toasts = live_toasts(state, now);
    if toasts.is_empty() {
        return;
    }

    // Anchor at the bottom row of the strip area: the newest toast sits
    // closest to the input.
    let rows = (toasts.len() as u16).min(area.height);
    let y0 = area.bottom() - rows;
    for (i, toast) in toasts.iter().take(rows as usize).enumerate() {
        let age_ms = now.saturating_duration_since(toast.shown_at).as_millis() as u64;
        let (sym, level_style) = match toast.level {
            ToastLevel::Success => (opts.glyphs.ok, opts.theme.green),
            ToastLevel::Error => (opts.glyphs.fail, opts.theme.red),
            ToastLevel::Warn => ("!", opts.theme.accent),
            ToastLevel::Info => (opts.glyphs.main_dot, opts.theme.cyan),
        };
        // The level glyph is a badge, like a tool call's outcome. Fading
        // flattens every span to faint, the plaque included.
        let fading = is_fading(age_ms);
        let (sym_style, title_style, detail_style) = if fading {
            (opts.theme.faint, opts.theme.faint, opts.theme.faint)
        } else {
            (
                opts.theme.badge(level_style),
                opts.theme.fg,
                opts.theme.faint,
            )
        };

        // One line: ` {glyph} ` {title}, plus ` · {detail}` when it fits.
        let sym_w = display_width(sym) + 3;
        let text_w = (area.width as usize).saturating_sub(sym_w);
        let combined = match toast.detail.as_deref() {
            Some(detail) => format!("{} · {detail}", toast.title),
            None => toast.title.clone(),
        };
        let trunc = truncate_to_width(&combined, text_w, true);
        // The detail span exists only where the title survived in full;
        // a cut inside the title keeps the whole line in the title style.
        let boundary = if trunc.starts_with(toast.title.as_str()) {
            toast.title.len()
        } else {
            trunc.len()
        };
        let (title_part, detail_part) = trunc.split_at(boundary);
        let mut spans = vec![
            Span::styled(format!(" {sym} "), sym_style),
            Span::raw(" "),
            Span::styled(title_part.to_string(), title_style),
        ];
        if !detail_part.is_empty() {
            spans.push(Span::styled(detail_part.to_string(), detail_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, y0 + i as u16, area.width, 1),
        );
    }
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod toast_tests;
