//! Toasts (spec §7): up to 2 lines, right-aligned above the input, 6 s TTL:
//! `✓ cargo build finished 3m02s` + `warnings 2 · . to open`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::state::ToastLevel;

use crate::state::AppState;

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

    let (sym, style) = match toast.level {
        ToastLevel::Success => (opts.glyphs.ok, opts.theme.green),
        ToastLevel::Error => (opts.glyphs.fail, opts.theme.red),
        ToastLevel::Warn => ("!", opts.theme.accent),
        ToastLevel::Info => (opts.glyphs.main_dot, opts.theme.cyan),
    };

    let mut lines = Vec::new();
    let sym_w = pacode_render::display_width(sym) + 1;
    let title_line = Line::from(vec![
        Span::styled(format!("{sym} "), style),
        Span::styled(
            truncate_to_width(
                &toast.title,
                (area.width as usize).saturating_sub(sym_w),
                true,
            ),
            opts.theme.bold,
        ),
    ]);
    lines.push(title_line);

    if area.height > 1 {
        let detail_str = toast.detail.as_deref().unwrap_or(". to open");
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                truncate_to_width(detail_str, (area.width as usize).saturating_sub(2), true),
                opts.theme.faint,
            ),
        ]));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines), area);
}
