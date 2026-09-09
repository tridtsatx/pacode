//! Bordered popup overlay for remote clipboard hints (centered, ~50 cols, 4 lines).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use pacode_render::RenderOptions;

pub const POPUP_TOAST_TITLE: &str = "OSC52_HINT";

/// Draws a centered ~50 cols, 4 lines bordered popup with the given message.
pub fn draw(frame: &mut Frame, area: Rect, text: &str, opts: &RenderOptions) {
    if area.width < 10 || area.height < 4 {
        return;
    }

    let w = (50.min(area.width.saturating_sub(2))).max(10);
    let h = (4.min(area.height.saturating_sub(2))).max(2);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(opts.theme.accent);

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let paragraph = Paragraph::new(text)
        .style(opts.theme.fg)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, inner);
}
