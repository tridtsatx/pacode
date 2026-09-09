//! In-place selectors replacing the input area and footer (bottom of dialog column).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use codeapp_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    frame.render_widget(Clear, area);

    match &state.focus {
        Focus::Overlay(Overlay::EffortPicker { index }) => {
            draw_effort(frame, area, state, *index, opts);
        }
        Focus::Overlay(Overlay::ModePicker { index }) => {
            draw_mode(frame, area, state, *index, opts);
        }
        Focus::Overlay(Overlay::ModelPicker { query, index }) => {
            draw_model(frame, area, state, query, *index, opts);
        }
        Focus::Overlay(Overlay::ConfigPicker {
            index,
            editing_number,
        }) => {
            draw_config(frame, area, state, *index, editing_number.as_deref(), opts);
        }
        _ => {}
    }
}

fn draw_effort(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: usize,
    opts: &RenderOptions,
) {
    let levels = ["low", "medium", "high", "max"];
    let descriptions = [
        "low: fastest, minimal reasoning",
        "medium: balanced",
        "high: deep reasoning for hard tasks",
        "max: maximum reasoning, slow and expensive",
    ];

    let mut lines = Vec::new();

    // 0: Title
    lines.push(Line::from(vec![Span::styled(
        "Effort",
        opts.theme.bold.patch(opts.theme.accent),
    )]));

    // 1: Track `Faster ────────▲──── Smarter`
    let track_w = (area.width as usize).saturating_sub(18).max(12);
    let arrow_pos = match selected {
        0 => 0,
        1 => track_w / 3,
        2 => (track_w * 2) / 3,
        _ => track_w.saturating_sub(1),
    };
    let mut track_spans = vec![Span::styled("Faster ", opts.theme.dim)];
    for col in 0..track_w {
        if col == arrow_pos {
            let pointer = if state.config.ui.ascii_only {
                "^"
            } else {
                "▲"
            };
            track_spans.push(Span::styled(
                pointer,
                opts.theme.accent.patch(opts.theme.bold),
            ));
        } else {
            track_spans.push(Span::styled(opts.glyphs.hline, opts.theme.faint));
        }
    }
    track_spans.push(Span::styled(" Smarter", opts.theme.dim));
    lines.push(Line::from(track_spans));

    // 2: Options row `low   medium   high   max`
    let mut opt_spans = Vec::new();
    for (i, lvl) in levels.iter().enumerate() {
        let is_sel = i == selected;
        let style = if is_sel {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        opt_spans.push(Span::styled(*lvl, style));
        if i + 1 < levels.len() {
            opt_spans.push(Span::raw("   "));
        }
    }
    lines.push(Line::from(opt_spans));

    // 3: Description
    let desc = descriptions.get(selected).copied().unwrap_or("");
    lines.push(Line::from(Span::styled(desc, opts.theme.fg)));

    // 4: Blank
    lines.push(Line::default());

    // 5: Blank
    lines.push(Line::default());

    // 6: Hint
    let hint = if state.config.ui.ascii_only {
        "<-/-> adjust . enter confirm . esc cancel"
    } else {
        "←/→ adjust · enter confirm · esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_mode(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: usize,
    opts: &RenderOptions,
) {
    let modes = ["Build", "Auto", "Plan", "Bypass"];
    let descriptions = [
        "ask before edits and commands",
        "accept workspace edits, ask for commands",
        "read-only, plan mode",
        "bypass permissions on",
    ];

    let mut lines = Vec::new();

    // 0: Title
    lines.push(Line::from(vec![Span::styled(
        "Mode",
        opts.theme.bold.patch(opts.theme.accent),
    )]));

    // 1: Track `Strict ────────▲──── Unrestricted`
    let track_w = (area.width as usize).saturating_sub(24).max(12);
    let arrow_pos = match selected {
        0 => 0,
        1 => track_w / 3,
        2 => (track_w * 2) / 3,
        _ => track_w.saturating_sub(1),
    };
    let mut track_spans = vec![Span::styled("Strict ", opts.theme.dim)];
    for col in 0..track_w {
        if col == arrow_pos {
            let pointer = if state.config.ui.ascii_only {
                "^"
            } else {
                "▲"
            };
            track_spans.push(Span::styled(
                pointer,
                opts.theme.accent.patch(opts.theme.bold),
            ));
        } else {
            track_spans.push(Span::styled(opts.glyphs.hline, opts.theme.faint));
        }
    }
    track_spans.push(Span::styled(" Unrestricted", opts.theme.dim));
    lines.push(Line::from(track_spans));

    // 2: Options row `Build   Auto   Plan   Bypass`
    let mut opt_spans = Vec::new();
    for (i, mode_name) in modes.iter().enumerate() {
        let is_sel = i == selected;
        let style = if is_sel {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        opt_spans.push(Span::styled(*mode_name, style));
        if i + 1 < modes.len() {
            opt_spans.push(Span::raw("   "));
        }
    }
    lines.push(Line::from(opt_spans));

    // 3: Description
    let desc = descriptions.get(selected).copied().unwrap_or("");
    lines.push(Line::from(Span::styled(desc, opts.theme.fg)));

    // 4: Blank
    lines.push(Line::default());

    // 5: Blank
    lines.push(Line::default());

    // 6: Hint
    let hint = if state.config.ui.ascii_only {
        "<-/-> adjust . enter confirm . esc cancel"
    } else {
        "←/→ adjust · enter confirm · esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_model(
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
        "Switch Model",
        opts.theme.bold.patch(opts.theme.accent),
    )));

    // 1: Query input
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.dim),
        Span::styled(query, opts.theme.fg),
    ]));

    let query_lower = query.to_lowercase();
    let filtered: Vec<_> = state
        .models
        .iter()
        .filter(|m| {
            query_lower.is_empty()
                || m.route.to_string().to_lowercase().contains(&query_lower)
                || m.display_name.to_lowercase().contains(&query_lower)
        })
        .collect();

    let list_height = (area.height as usize).saturating_sub(3).min(8);

    if state.models.is_empty() {
        lines.push(Line::from(Span::styled("  loading…", opts.theme.dim)));
    } else if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching models",
            opts.theme.dim,
        )));
    } else {
        let sel_idx = if filtered.is_empty() {
            0
        } else {
            selected % filtered.len()
        };
        let start = if sel_idx >= list_height {
            sel_idx + 1 - list_height
        } else {
            0
        };

        for (i, m) in filtered.iter().enumerate().skip(start).take(list_height) {
            let is_sel = i == sel_idx;
            let route_str = m.route.to_string();
            let disp = format!("{route_str}  {}", m.display_name);
            let line_text = format!(
                "{:<width$}",
                disp,
                width = (area.width as usize).saturating_sub(2)
            );

            if is_sel {
                lines.push(Line::from(Span::styled(
                    format!("▸ {line_text}"),
                    opts.theme
                        .selected_bg
                        .patch(opts.theme.bold)
                        .patch(opts.theme.accent),
                )));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(route_str, opts.theme.fg),
                    Span::raw("  "),
                    Span::styled(&m.display_name, opts.theme.dim),
                ]));
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

    // Set cursor on query input
    let cursor_x = area.x + 2 + (query.len() as u16).min(area.width.saturating_sub(3));
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn draw_config(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: usize,
    editing_number: Option<&str>,
    opts: &RenderOptions,
) {
    let mut lines = Vec::new();

    // 0: Title
    lines.push(Line::from(Span::styled(
        "Quick Settings",
        opts.theme.bold.patch(opts.theme.accent),
    )));

    let model_val = state
        .model()
        .map(|m| m.to_string())
        .or_else(|| state.config.provider.default.clone())
        .unwrap_or_else(|| "default".to_string());
    let effort_val = state.effort().as_str().to_string();
    let mode_val = state.mode().as_str().to_string();
    let mouse_val = state.config.ui.mouse.to_string();
    let ascii_val = state.config.ui.ascii_only.to_string();
    let hints_effort_val = state.config.ui.hints.effort.to_string();
    let color_val = state.config.ui.color.clone();
    let yield_val = state.config.exec.yield_after_secs.to_string();
    let agents_val = state.config.agents.max_live.to_string();
    let idle_val = state.config.daemon.idle_timeout_secs.to_string();

    let items = [
        ("model", model_val),
        ("effort", effort_val),
        ("mode", mode_val),
        ("ui.mouse", mouse_val),
        ("ui.ascii_only", ascii_val),
        ("ui.hints.effort", hints_effort_val),
        ("ui.color", color_val),
        ("exec.yield_after_secs", yield_val),
        ("agents.max_live", agents_val),
        ("daemon.idle_timeout_secs", idle_val),
    ];

    let mut edit_cursor: Option<(u16, u16)> = None;

    for (i, (key, val)) in items.iter().enumerate() {
        if lines.len() >= (area.height as usize).saturating_sub(1) {
            break;
        }

        let is_sel = i == selected;
        if is_sel {
            if let Some(buf) = editing_number {
                let line_y = area.y + lines.len() as u16;
                let cursor_x = area.x + 6 + key.len() as u16 + buf.len() as u16;
                edit_cursor = Some((cursor_x, line_y));
                lines.push(Line::from(vec![
                    Span::styled("▸ ", opts.theme.accent),
                    Span::styled(*key, opts.theme.bold),
                    Span::raw(": [ "),
                    Span::styled(buf, opts.theme.accent.patch(opts.theme.bold)),
                    Span::raw(" ]"),
                ]));
            } else {
                let text = format!("▸ {key}: {val}");
                let padded = format!(
                    "{:<width$}",
                    text,
                    width = (area.width as usize).saturating_sub(1)
                );
                lines.push(Line::from(Span::styled(
                    padded,
                    opts.theme
                        .selected_bg
                        .patch(opts.theme.bold)
                        .patch(opts.theme.accent),
                )));
            }
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(*key, opts.theme.fg),
                Span::raw(": "),
                Span::styled(val, opts.theme.dim),
            ]));
        }
    }

    while lines.len() < (area.height as usize).saturating_sub(1) {
        lines.push(Line::default());
    }

    let hint = if editing_number.is_some() {
        "enter confirm · esc cancel"
    } else if state.config.ui.ascii_only {
        "^/v select . enter/<-/-> edit . esc close"
    } else {
        "↑/↓ select · enter/←/→ edit · esc close"
    };
    lines.push(Line::from(Span::styled(hint, opts.theme.faint)));

    frame.render_widget(Paragraph::new(lines), area);

    if let Some((cx, cy)) = edit_cursor {
        frame.set_cursor_position((cx, cy));
    }
}
