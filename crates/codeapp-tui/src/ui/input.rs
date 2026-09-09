//! The prompt (spec §3): thin line above, `❯` dim + text + block cursor, thin line
//! below; no background, no border. Multi-line up to 6 rows. Slash popup above the
//! input (max 6 rows) listing matching commands with usage and help.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use codeapp_render::RenderOptions;

use crate::commands;
use crate::layout::ScreenLayout;
use crate::state::AppState;

pub fn draw(frame: &mut Frame, layout: &ScreenLayout, state: &mut AppState, opts: &RenderOptions) {
    if layout.input_top.height > 0 {
        let hline = opts.glyphs.hline.repeat(layout.input_top.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hline, opts.theme.faint))),
            layout.input_top,
        );
    }

    if layout.input.height > 0 {
        let lines = state.input.wrap_lines(layout.input.width);
        let total_lines = lines.len();
        let height = layout.input.height as usize;
        let (cur_line, cur_col) = state.input.cursor_position(layout.input.width);

        let max_scroll = total_lines.saturating_sub(height);
        state.input.input_scroll = state.input.input_scroll.min(max_scroll);

        let cur_line_idx = cur_line as usize;
        if cur_line_idx < state.input.input_scroll {
            state.input.input_scroll = cur_line_idx;
        } else if cur_line_idx >= state.input.input_scroll + height {
            state.input.input_scroll = cur_line_idx.saturating_sub(height.saturating_sub(1));
        }

        let mut rendered = Vec::new();
        for (i, wl) in lines
            .iter()
            .enumerate()
            .skip(state.input.input_scroll)
            .take(height)
        {
            let prefix = if i == 0 {
                Span::styled(format!("{} ", opts.glyphs.prompt), opts.theme.dim)
            } else {
                Span::raw("  ")
            };
            rendered.push(Line::from(vec![
                prefix,
                Span::styled(wl.clone(), opts.theme.fg),
            ]));
        }

        frame.render_widget(Paragraph::new(rendered), layout.input);

        let cur_screen_line = cur_line.saturating_sub(state.input.input_scroll as u16);
        if cur_line >= state.input.input_scroll as u16 && (cur_screen_line as usize) < height {
            frame.set_cursor_position((layout.input.x + cur_col, layout.input.y + cur_screen_line));
        }
    }

    if layout.input_bottom.height > 0 {
        let hline = opts.glyphs.hline.repeat(layout.input_bottom.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hline, opts.theme.faint))),
            layout.input_bottom,
        );
    }

    if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
        let query = &state.input.text[1..];
        let matches = commands::matching(query);
        if !matches.is_empty() {
            let count = matches.len().min(6);
            let popup_h = count as u16;
            let popup_y = layout.input_top.y.saturating_sub(popup_h);
            let popup_w = layout.input.width.min(50);
            let popup_area = Rect::new(layout.input.x, popup_y, popup_w, popup_h);

            frame.render_widget(Clear, popup_area);

            let selected = state.input.slash_index % matches.len();
            let mut popup_lines = Vec::new();

            for (i, cmd) in matches.iter().enumerate().take(count) {
                let is_sel = i == selected;
                let (usage_style, help_style) = if is_sel {
                    (opts.theme.selected_bg, opts.theme.selected_bg)
                } else {
                    (opts.theme.accent, opts.theme.dim)
                };

                let usage = format!("{:<16}", cmd.usage);
                let line = Line::from(vec![
                    Span::styled(usage, usage_style),
                    Span::raw(" "),
                    Span::styled(cmd.help, help_style),
                ]);
                popup_lines.push(line);
            }

            frame.render_widget(Paragraph::new(popup_lines), popup_area);
        }
    }
}
