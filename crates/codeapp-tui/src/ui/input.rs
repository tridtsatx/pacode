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

        let matching_cmd_token = if state.input.text.starts_with('/') {
            let token = state
                .input
                .text
                .split_whitespace()
                .next()
                .unwrap_or(&state.input.text);
            let name = token.strip_prefix('/').unwrap_or("");
            if commands::COMMANDS.iter().any(|c| c.name == name) {
                Some(token.to_string())
            } else {
                None
            }
        } else {
            None
        };

        let inline_hint = command_inline_hint(&state.input.text);

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

            let mut line_spans = vec![prefix];
            if i == 0 {
                if let Some(ref token) = matching_cmd_token
                    && wl.starts_with(token.as_str())
                {
                    line_spans.push(Span::styled(token.clone(), opts.theme.cyan));
                    line_spans.push(Span::styled(wl[token.len()..].to_string(), opts.theme.fg));
                } else {
                    line_spans.push(Span::styled(wl.clone(), opts.theme.fg));
                }
                if let Some(ref hint) = inline_hint {
                    line_spans.push(Span::styled(hint.clone(), opts.theme.dim));
                }
            } else {
                line_spans.push(Span::styled(wl.clone(), opts.theme.fg));
            }

            rendered.push(Line::from(line_spans));
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
            let max_visible = 6;
            let count = matches.len().min(max_visible);
            let popup_h = count as u16;
            let popup_y = layout.input_top.y.saturating_sub(popup_h);
            let popup_w = layout.input.width.min(50);
            let popup_area = Rect::new(layout.input.x, popup_y, popup_w, popup_h);

            frame.render_widget(Clear, popup_area);

            let selected = state.input.slash_index % matches.len();
            let start = if selected >= max_visible {
                selected + 1 - max_visible
            } else {
                0
            };

            let mut popup_lines = Vec::new();
            for (i, cmd) in matches.iter().enumerate().skip(start).take(count) {
                let is_sel = i == selected;
                let usage_style = if is_sel {
                    opts.theme
                        .selected_bg
                        .patch(opts.theme.cyan)
                        .patch(opts.theme.bold)
                } else {
                    opts.theme.accent
                };
                let help_style = if is_sel {
                    opts.theme.selected_bg.patch(opts.theme.fg)
                } else {
                    opts.theme.dim
                };

                let usage = format!("{:<12}", cmd.usage);
                let line = if is_sel {
                    let text = format!("{usage} {}", cmd.help);
                    let padded = format!("{:<width$}", text, width = popup_w as usize);
                    Line::from(Span::styled(
                        padded,
                        opts.theme
                            .selected_bg
                            .patch(opts.theme.cyan)
                            .patch(opts.theme.bold),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(usage, usage_style),
                        Span::raw(" "),
                        Span::styled(cmd.help, help_style),
                    ])
                };
                popup_lines.push(line);
            }

            frame.render_widget(Paragraph::new(popup_lines), popup_area);
        }
    }
}

pub fn command_inline_hint(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix('/') {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() == 1 {
            let cmd_name = parts[0];
            if let Some(cmd) = commands::COMMANDS.iter().find(|c| c.name == cmd_name)
                && !cmd.arg_hint.is_empty()
            {
                if text.ends_with(' ') {
                    return Some(cmd.arg_hint.to_string());
                } else {
                    return Some(format!(" {}", cmd.arg_hint));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_inline_hints() {
        assert_eq!(
            command_inline_hint("/effort"),
            Some(" [low|medium|high|xhigh|max]".into())
        );
        assert_eq!(
            command_inline_hint("/effort "),
            Some("[low|medium|high|xhigh|max]".into())
        );
        assert_eq!(
            command_inline_hint("/mode"),
            Some(" [build|auto|plan|bypass]".into())
        );
        assert_eq!(
            command_inline_hint("/mode "),
            Some("[build|auto|plan|bypass]".into())
        );
        assert_eq!(
            command_inline_hint("/model"),
            Some(" [provider/model]".into())
        );
        assert_eq!(
            command_inline_hint("/model "),
            Some("[provider/model]".into())
        );
        assert_eq!(command_inline_hint("/export"), Some(" [path]".into()));
        assert_eq!(command_inline_hint("/export "), Some("[path]".into()));

        // Commands with no argument hint
        assert_eq!(command_inline_hint("/config"), None);
        assert_eq!(command_inline_hint("/config "), None);

        // Commands with arguments already typed
        assert_eq!(command_inline_hint("/effort hi"), None);
        assert_eq!(command_inline_hint("/mode plan"), None);

        // Unknown or non-slash inputs
        assert_eq!(command_inline_hint("/unknown"), None);
        assert_eq!(command_inline_hint("hello"), None);
        assert_eq!(command_inline_hint(""), None);
    }
}
