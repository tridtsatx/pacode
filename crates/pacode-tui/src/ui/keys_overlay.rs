//! Keybindings overlay (`/keys`). Shows actions, current bindings, and allows rebinding.

#[cfg(test)]
#[path = "keys_overlay_tests.rs"]
mod keys_overlay_tests;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::binding::{Keymap, format_binding};
use crate::state::AppState;
use crate::ui::Frame;
use pacode_render::RenderOptions;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    selected: usize,
    capturing: bool,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Keyboard Shortcuts ", opts.theme.bold))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 2 {
        return;
    }

    let actions = Keymap::action_names();
    let footer_height = 1;
    let max_rows = (inner.height as usize).saturating_sub(footer_height);
    let sel_idx = selected.min(actions.len().saturating_sub(1));
    let start = if max_rows > 0 && sel_idx >= max_rows {
        sel_idx + 1 - max_rows
    } else {
        0
    };

    let mut lines = Vec::new();

    for (i, &(_name, action)) in actions.iter().enumerate().skip(start).take(max_rows) {
        let is_sel = i == sel_idx;
        let pointer = if is_sel { opts.glyphs.pointer } else { " " };
        let pointer_style = if is_sel {
            opts.theme.accent
        } else {
            opts.theme.fg
        };

        let is_overridden = state.keymap.is_overridden(action);
        let desc = action.description();

        let (right_str, right_style) = if is_sel && capturing {
            let s = if opts.glyphs.ascii {
                "press a key...".to_string()
            } else {
                "press a key…".to_string()
            };
            (s, opts.theme.accent.patch(opts.theme.bold))
        } else {
            let bindings = state.keymap.bindings_for(action);
            let joined = bindings
                .iter()
                .map(format_binding)
                .collect::<Vec<_>>()
                .join(", ");
            if is_overridden {
                (format!("{joined} *"), opts.theme.cyan)
            } else {
                (joined, opts.theme.dim)
            }
        };

        let desc_style = if is_sel {
            opts.theme.selected_bg.patch(opts.theme.bold)
        } else {
            opts.theme.fg
        };

        let right_w = pacode_render::display_width(&right_str);
        let avail_desc_w = (inner.width as usize).saturating_sub(right_w + 3);
        let desc_display = pacode_render::truncate_to_width(desc, avail_desc_w, opts.glyphs.ascii);
        let left_w = 2 + pacode_render::display_width(&desc_display);
        let padding = (inner.width as usize).saturating_sub(left_w + right_w);

        lines.push(Line::from(vec![
            Span::styled(pointer, pointer_style),
            Span::raw(" "),
            Span::styled(desc_display, desc_style),
            Span::raw(" ".repeat(padding)),
            Span::styled(right_str, right_style),
        ]));
    }

    while lines.len() < (inner.height as usize).saturating_sub(1) {
        lines.push(Line::default());
    }

    let footer_hint = if capturing {
        if opts.glyphs.ascii {
            "press key to bind . esc cancel"
        } else {
            "press key to bind · esc cancel"
        }
    } else if opts.glyphs.ascii {
        "^/v select . enter rebind . ctrl+r reset . esc close"
    } else {
        "↑/↓ select · enter rebind · ctrl+r reset · esc close"
    };
    lines.push(Line::from(Span::styled(footer_hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), inner);
}
