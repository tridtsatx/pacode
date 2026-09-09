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

    match step {
        ThemePickerStep::SelectTheme => {
            // Title
            lines.push(Line::from(Span::styled(
                "Color Theme",
                opts.theme.bold.patch(opts.theme.accent),
            )));

            let mut rows: Vec<String> = Vec::new();
            for p in builtins {
                if p.light {
                    let name = &p.name;
                    rows.push(format!("{name} (built-in, light)"));
                } else {
                    let name = &p.name;
                    rows.push(format!("{name} (built-in)"));
                }
            }
            for u in user_themes {
                rows.push(format!("{u} (user)"));
            }
            rows.push("Create new one…".to_string());

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

            for (i, row_text) in rows.iter().enumerate().skip(start).take(list_height) {
                let is_sel = i == sel_idx;
                let padded = format!(
                    "{:<width$}",
                    row_text,
                    width = (area.width as usize).saturating_sub(2)
                );
                if is_sel {
                    lines.push(Line::from(Span::styled(
                        format!("▸ {padded}"),
                        opts.theme
                            .selected_bg
                            .patch(opts.theme.bold)
                            .patch(opts.theme.accent),
                    )));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(row_text.clone(), opts.theme.fg),
                    ]));
                }
            }
        }
        ThemePickerStep::SelectBase => {
            // Title
            lines.push(Line::from(Span::styled(
                "Select Base Theme",
                opts.theme.bold.patch(opts.theme.accent),
            )));

            let rows: Vec<String> = builtins
                .iter()
                .map(|p| {
                    if p.light {
                        let name = &p.name;
                        format!("{name} (light)")
                    } else {
                        p.name.clone()
                    }
                })
                .collect();

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

            for (i, row_text) in rows.iter().enumerate().skip(start).take(list_height) {
                let is_sel = i == sel_idx;
                let padded = format!(
                    "{:<width$}",
                    row_text,
                    width = (area.width as usize).saturating_sub(2)
                );
                if is_sel {
                    lines.push(Line::from(Span::styled(
                        format!("▸ {padded}"),
                        opts.theme
                            .selected_bg
                            .patch(opts.theme.bold)
                            .patch(opts.theme.accent),
                    )));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(row_text.clone(), opts.theme.fg),
                    ]));
                }
            }
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
    lines.push(Line::from(Span::styled(hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
#[path = "theme_picker_tests.rs"]
mod theme_picker_tests;
