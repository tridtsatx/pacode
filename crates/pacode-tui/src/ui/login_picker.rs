//! Login picker bottom overlay for selecting a provider and authenticating.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::state::AppState;
use crate::ui::overlays::selected_row;

#[cfg(test)]
#[path = "login_picker_tests.rs"]
mod login_picker_tests;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    query: &str,
    selected: usize,
    opts: &RenderOptions,
) {
    let mut lines = Vec::new();

    // 0: Title
    lines.push(Line::from(Span::styled(
        "Sign in to Provider",
        opts.theme.bold.patch(opts.theme.accent),
    )));

    // 1: Query input
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.accent),
        Span::styled(query.to_string(), opts.theme.fg),
    ]));

    let query_lower = query.to_lowercase();
    let filtered: Vec<_> = state
        .auth_providers
        .iter()
        .filter(|p| {
            query_lower.is_empty()
                || p.id.to_lowercase().contains(&query_lower)
                || p.display_name.to_lowercase().contains(&query_lower)
                || p.auth_kind.to_lowercase().contains(&query_lower)
                || p.detail.to_lowercase().contains(&query_lower)
        })
        .collect();

    let list_height = (area.height as usize).saturating_sub(4).min(8);

    let sel_provider = if filtered.is_empty() {
        None
    } else {
        let sel_idx = selected % filtered.len();
        Some(filtered[sel_idx])
    };

    if state.auth_providers.is_empty() {
        lines.push(Line::from(Span::styled("  loading…", opts.theme.faint)));
    } else if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching providers",
            opts.theme.dim,
        )));
        lines.push(Line::from(Span::styled(
            "  backspace to clear the filter",
            opts.theme.faint,
        )));
    } else {
        let sel_idx = selected % filtered.len();
        let start = if sel_idx >= list_height {
            sel_idx + 1 - list_height
        } else {
            0
        };

        for (i, p) in filtered.iter().enumerate().skip(start).take(list_height) {
            let is_sel = i == sel_idx;
            let (glyph, status_style) = match &p.state {
                pacode_types::AuthState::Configured => (opts.glyphs.ok, opts.theme.green),
                pacode_types::AuthState::NeedsAttention { .. } => {
                    (opts.glyphs.fail, opts.theme.yellow)
                }
                pacode_types::AuthState::NotConfigured => (opts.glyphs.disabled, opts.theme.dim),
            };

            let mut spans = vec![
                Span::styled(
                    format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
                    opts.theme.accent,
                ),
                Span::styled(format!("{glyph} "), status_style),
                Span::styled(
                    p.display_name.clone(),
                    if is_sel {
                        opts.theme.bold
                    } else {
                        opts.theme.fg
                    },
                ),
                Span::raw("  "),
                Span::styled(format!("[{}]", p.auth_kind), opts.theme.cyan),
            ];
            if p.recommended {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("(recommended)", opts.theme.faint));
            }
            let line = Line::from(spans);
            if is_sel {
                lines.push(selected_row(line, area.width, opts));
            } else {
                lines.push(line);
            }
        }
    }

    // Detail line under the list
    if let Some(p) = sel_provider {
        let mut detail_parts = Vec::new();
        if !p.detail.is_empty() {
            detail_parts.push(p.detail.clone());
        }
        if p.accounts.is_empty() {
            detail_parts.push("no accounts stored".to_string());
        } else {
            let active_label = p.active.as_deref().unwrap_or("");
            let acc_list: Vec<String> = p
                .accounts
                .iter()
                .map(|a| {
                    if a == active_label {
                        format!("*{a} (active)")
                    } else {
                        a.clone()
                    }
                })
                .collect();
            detail_parts.push(format!("accounts: {}", acc_list.join(", ")));
        }
        if let pacode_types::AuthState::NeedsAttention { reason } = &p.state {
            detail_parts.push(format!("needs attention: {reason}"));
        }
        let sep = if state.config.ui.ascii_only {
            " . "
        } else {
            " · "
        };
        let full_detail = format!("  {}", detail_parts.join(sep));
        let truncated =
            pacode_render::truncate_to_width(&full_detail, area.width as usize, opts.glyphs.ascii);
        lines.push(Line::from(Span::styled(truncated, opts.theme.dim)));
    }

    while lines.len() < (area.height as usize).saturating_sub(1) {
        lines.push(Line::default());
    }

    let sep = if state.config.ui.ascii_only {
        " . "
    } else {
        " · "
    };
    let mut hint_parts = vec![
        if state.config.ui.ascii_only {
            "^/v select"
        } else {
            "↑/↓ select"
        },
        "enter login",
    ];
    if sel_provider.is_some_and(|p| p.accounts.len() > 1) {
        hint_parts.push("a switch account");
    }
    hint_parts.push("ctrl+d logout");
    hint_parts.push("esc cancel");
    let hint = hint_parts.join(sep);
    lines.push(Line::from(Span::styled(hint, opts.theme.dim)));

    frame.render_widget(Paragraph::new(lines), area);

    // Set cursor on query input
    let cursor_x = area.x + 2 + (query.len() as u16).min(area.width.saturating_sub(3));
    frame.set_cursor_position((cursor_x, area.y + 1));
}
