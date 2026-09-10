//! Theme picker overlay.
//!
//! Two steps:
//! 1. Select existing theme (built-in or user) or choose "Create new one…".
//! 2. If creating new, pick a built-in as base palette.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay, ThemePickerStep};
use crate::ui::overlays::selected_row;

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    let Focus::Overlay(Overlay::ThemePicker {
        index,
        step,
        user_themes,
        ..
    }) = &state.focus
    else {
        return;
    };

    let builtins = pacode_render::builtin_palettes();
    let mut lines = Vec::new();

    // (title, rows) per step; a row is the label plus the dim tag behind it.
    let (title, rows): (&str, Vec<(String, &str)>) = match step {
        ThemePickerStep::SelectTheme => {
            let mut rows: Vec<(String, &str)> = Vec::new();
            for p in builtins {
                let tag = if p.light {
                    "built-in · light"
                } else {
                    "built-in"
                };
                rows.push((p.name.clone(), tag));
            }
            for u in user_themes {
                rows.push((u.clone(), "user"));
            }
            let create = if opts.glyphs.ascii {
                "Create new one...".to_string()
            } else {
                "Create new one…".to_string()
            };
            rows.push((create, ""));
            ("Color Theme", rows)
        }
        ThemePickerStep::SelectBase => (
            "Select Base Theme",
            builtins
                .iter()
                .map(|p| {
                    let tag = if p.light { "light" } else { "" };
                    (p.name.clone(), tag)
                })
                .collect(),
        ),
    };

    // 0: Title
    lines.push(Line::from(Span::styled(
        title,
        opts.theme.bold.patch(opts.theme.accent),
    )));

    let list_height = (area.height as usize).saturating_sub(2).min(8);
    let sel_idx = if rows.is_empty() {
        0
    } else {
        index % rows.len()
    };
    let start = if sel_idx >= list_height {
        sel_idx + 1 - list_height
    } else {
        0
    };

    for (i, (name, tag)) in rows.iter().enumerate().skip(start).take(list_height) {
        let is_sel = i == sel_idx;
        let mut spans = vec![Span::styled(
            format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
            opts.theme.accent,
        )];
        spans.push(Span::styled(
            name.clone(),
            if is_sel {
                opts.theme.bold
            } else {
                opts.theme.fg
            },
        ));
        if !tag.is_empty() {
            spans.push(Span::styled(
                format!("{sep}{tag}", sep = opts.glyphs.dot_sep),
                opts.theme.faint,
            ));
        }
        let line = Line::from(spans);
        if is_sel {
            lines.push(selected_row(line, area.width, opts));
        } else {
            lines.push(line);
        }
    }

    while lines.len() < (area.height as usize).saturating_sub(1) {
        lines.push(Line::default());
    }

    let hint = if state.config.ui.ascii_only {
        "^/v select . enter confirm . esc cancel"
    } else {
        "↑/↓ select · enter confirm · esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, opts.theme.dim)));

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
#[path = "theme_picker_tests.rs"]
mod theme_picker_tests;
