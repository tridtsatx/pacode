//! Toasts (spec §7): up to 2 lines, right-aligned above the input, 6 s TTL:
//! `✓ cargo build завершилась 3m02s` + `warnings 2 · точка — открыть`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use codeapp_render::{RenderOptions, truncate_to_width};
use codeapp_types::state::ToastLevel;

use crate::state::AppState;

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(toast) = state.toasts.back() else {
        return;
    };

    let (sym, style) = match toast.level {
        ToastLevel::Success => (opts.glyphs.ok, opts.theme.green),
        ToastLevel::Error => (opts.glyphs.fail, opts.theme.red),
        ToastLevel::Warn => ("!", opts.theme.accent),
        ToastLevel::Info => (opts.glyphs.main_dot, opts.theme.cyan),
    };

    let mut lines = Vec::new();
    let title_line = Line::from(vec![
        Span::styled(format!("{sym} "), style),
        Span::styled(
            truncate_to_width(&toast.title, (area.width as usize).saturating_sub(4), true),
            opts.theme.bold,
        ),
    ]);
    lines.push(title_line);

    if area.height > 1 {
        let detail_str = toast.detail.as_deref().unwrap_or(". — открыть");
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                truncate_to_width(detail_str, (area.width as usize).saturating_sub(4), true),
                opts.theme.faint,
            ),
        ]));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines), area);
}
