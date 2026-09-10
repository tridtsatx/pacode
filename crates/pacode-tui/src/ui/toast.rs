//! Toasts (spec §7): up to 2 lines, right-aligned above the input, 6 s TTL:
//! `✓ cargo build finished 3m02s` + `warnings 2`.
//!
//! Two bounded animations ride the anim tick while a toast is young or about
//! to expire: a slide-in from the right edge over `TOAST_ENTER_MS` with a faint
//! border, and a fade to `theme.faint` over the last `TOAST_EXIT_MS` of life.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::state::ToastLevel;

use crate::state::{AppState, TOAST_ENTER_MS, TOAST_EXIT_MS, TOAST_TTL_SECS};

/// Cells of left-edge travel remaining at `age_ms`: an ease-out slide-in over
/// `TOAST_ENTER_MS` covering four cells. Pure, so tests can pin the curve.
pub fn enter_offset(age_ms: u64) -> u16 {
    if age_ms >= TOAST_ENTER_MS {
        return 0;
    }
    let t = age_ms as f32 / TOAST_ENTER_MS as f32;
    let eased = 1.0 - (1.0 - t) * (1.0 - t);
    ((1.0 - eased) * 4.0).round() as u16
}

/// Whether a toast of `age_ms` is inside its fade-out window.
pub fn is_fading(age_ms: u64) -> bool {
    (TOAST_TTL_SECS * 1000).saturating_sub(age_ms) < TOAST_EXIT_MS
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(toast) = state
        .toasts
        .iter()
        .rev()
        .find(|t| t.title != crate::ui::popup::POPUP_TOAST_TITLE)
    else {
        return;
    };

    let age_ms = std::time::Instant::now()
        .saturating_duration_since(toast.shown_at)
        .as_millis() as u64;

    // The slide-in narrows the toast toward its right edge — subtractive, so
    // nothing is ever drawn outside the layout's toast area.
    let off = enter_offset(age_ms);
    let area = Rect::new(
        area.x + off.min(area.width),
        area.y,
        area.width.saturating_sub(off),
        area.height,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (sym, style) = match toast.level {
        ToastLevel::Success => (opts.glyphs.ok, opts.theme.green),
        ToastLevel::Error => (opts.glyphs.fail, opts.theme.red),
        ToastLevel::Warn => ("!", opts.theme.accent),
        ToastLevel::Info => (opts.glyphs.main_dot, opts.theme.cyan),
    };

    // Fading flattens every span to faint; entering keeps the border faint
    // until the toast has landed.
    let fading = is_fading(age_ms);
    let (sym_style, border_style, title_style) = if fading {
        (opts.theme.faint, opts.theme.faint, opts.theme.faint)
    } else {
        let border = if off > 0 {
            opts.theme.faint
        } else {
            opts.theme.cyan
        };
        (style, border, opts.theme.bold)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);

    let mut lines = Vec::new();
    let sym_w = pacode_render::display_width(sym) + 1;
    let title_line = Line::from(vec![
        Span::styled(format!("{sym} "), sym_style),
        Span::styled(
            truncate_to_width(
                &toast.title,
                (inner.width as usize).saturating_sub(sym_w),
                true,
            ),
            title_style,
        ),
    ]);
    lines.push(title_line);

    if inner.height > 1
        && let Some(detail_str) = toast.detail.as_deref()
    {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                truncate_to_width(detail_str, (inner.width as usize).saturating_sub(2), true),
                opts.theme.faint,
            ),
        ]));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod toast_tests;
