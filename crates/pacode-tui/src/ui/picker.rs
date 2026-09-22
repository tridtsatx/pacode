//! In-place selectors replacing the input area and footer (bottom of dialog column).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay};
use crate::ui::overlays::selected_row;

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
        Focus::Overlay(Overlay::ModelPicker {
            query,
            index,
            for_zdr,
        }) => {
            draw_model(frame, area, state, query, *index, *for_zdr, opts);
        }
        Focus::Overlay(Overlay::LoginPicker { query, index }) => {
            crate::ui::login_picker::draw(frame, area, state, query, *index, opts);
        }
        Focus::Overlay(Overlay::QuestionPicker {
            question,
            index,
            selected,
            typed,
            typing,
        }) => {
            crate::ui::question_picker::draw(
                frame, area, question, *index, selected, typed, *typing, opts,
            );
        }
        Focus::Overlay(Overlay::ThemePicker { .. }) => {
            crate::ui::theme_picker::draw(frame, area, state, opts);
        }
        _ => {}
    }
}

pub fn picker_track_geometry(area_width: u16) -> (usize, usize) {
    let start = 8usize;
    let track_w = 48usize.min((area_width as usize).saturating_sub(20)).max(4);
    (start, track_w)
}

pub fn option_center_x(start: usize, track_w: usize, index: usize, num_options: usize) -> usize {
    if num_options <= 1 {
        start
    } else {
        start + index * (track_w.saturating_sub(1)) / (num_options - 1)
    }
}

pub fn option_label_start(center_x: usize, label_len: usize) -> usize {
    center_x.saturating_sub(label_len / 2)
}

pub fn option_label_center(label_start: usize, label_len: usize) -> usize {
    label_start + label_len / 2
}

fn draw_effort(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: usize,
    opts: &RenderOptions,
) {
    let levels = ["low", "medium", "high", "xhigh", "max"];
    let descriptions = [
        "low: fastest, minimal reasoning",
        "medium: balanced",
        "high: deep reasoning for hard tasks",
        "xhigh: extra reasoning; sent as `high` unless the provider maps it",
        "max: maximum reasoning, slow and expensive",
    ];
    // Greyed levels are unsupported by the current model and unselectable
    // (spec §6); `None` means no restriction.
    let supported = state.supported_efforts();
    let allowed = |i: usize| {
        pacode_types::Effort::ALL
            .get(i)
            .is_some_and(|e| supported.is_none_or(|l| l.contains(e)))
    };

    let mut lines = Vec::new();

    // 0: Title
    lines.push(Line::from(vec![Span::styled(
        "Effort",
        opts.theme.bold.patch(opts.theme.accent),
    )]));

    let (start, track_w) = picker_track_geometry(area.width);
    let selected_x = option_center_x(start, track_w, selected, levels.len());

    // 1: Track `Faster ────────▲──── Smarter`
    let pointer = if state.config.ui.ascii_only {
        "^"
    } else {
        "▲"
    };
    let mut track_spans = vec![Span::styled(
        format!("{:<start$}", "Faster "),
        opts.theme.dim,
    )];
    for col in start..(start + track_w) {
        if col == selected_x {
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
    let mut cur_col = 0usize;
    for (i, &lvl) in levels.iter().enumerate() {
        let center_x = option_center_x(start, track_w, i, levels.len());
        let label_start = option_label_start(center_x, lvl.len()).max(cur_col);
        if label_start > cur_col {
            opt_spans.push(Span::raw(" ".repeat(label_start - cur_col)));
        }
        let is_sel = i == selected;
        let style = if !allowed(i) {
            opts.theme.faint
        } else if is_sel {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        opt_spans.push(Span::styled(lvl, style));
        cur_col = label_start + lvl.len();
    }
    lines.push(Line::from(opt_spans));

    // 3: Description
    let desc = if allowed(selected) {
        descriptions.get(selected).copied().unwrap_or("")
    } else {
        "not supported by the current model"
    };
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
    lines.push(Line::from(Span::styled(hint, opts.theme.dim)));

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

    let (start, track_w) = picker_track_geometry(area.width);
    let selected_x = option_center_x(start, track_w, selected, modes.len());

    // 1: Track `Safer ────────▲──── Freer`
    let pointer = if state.config.ui.ascii_only {
        "^"
    } else {
        "▲"
    };
    let mut track_spans = vec![Span::styled(
        format!("{:<start$}", "Safer "),
        opts.theme.dim,
    )];
    for col in start..(start + track_w) {
        if col == selected_x {
            track_spans.push(Span::styled(
                pointer,
                opts.theme.accent.patch(opts.theme.bold),
            ));
        } else {
            track_spans.push(Span::styled(opts.glyphs.hline, opts.theme.faint));
        }
    }
    track_spans.push(Span::styled(" Freer", opts.theme.dim));
    lines.push(Line::from(track_spans));

    // 2: Options row `Build   Auto   Plan   Bypass`
    let mut opt_spans = Vec::new();
    let mut cur_col = 0usize;
    for (i, &mode_name) in modes.iter().enumerate() {
        let center_x = option_center_x(start, track_w, i, modes.len());
        let label_start = option_label_start(center_x, mode_name.len()).max(cur_col);
        if label_start > cur_col {
            opt_spans.push(Span::raw(" ".repeat(label_start - cur_col)));
        }
        let is_sel = i == selected;
        let style = if is_sel {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        opt_spans.push(Span::styled(mode_name, style));
        cur_col = label_start + mode_name.len();
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
    lines.push(Line::from(Span::styled(hint, opts.theme.dim)));

    frame.render_widget(Paragraph::new(lines), area);
}

#[allow(clippy::too_many_arguments)]
fn draw_model(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    query: &str,
    selected: usize,
    for_zdr: bool,
    opts: &RenderOptions,
) {
    let mut lines = Vec::new();

    // 0: Title — with the §8.2 cache badge when the model list on screen is a
    // stale disk copy or still loading.
    let base_title = if for_zdr {
        "ZDR: pick a capable model"
    } else {
        "Switch Model"
    };
    let title = crate::ui::overlays::title_with_badge(
        base_title,
        state.lists.badge(crate::cache::ListKind::Models),
    );
    lines.push(Line::from(Span::styled(
        title,
        opts.theme.bold.patch(opts.theme.accent),
    )));

    // 1: Query input
    lines.push(Line::from(vec![
        Span::styled("> ", opts.theme.accent),
        Span::styled(query.to_string(), opts.theme.fg),
    ]));

    let query_lower = query.to_lowercase();
    // ZDR-active sessions restrict the picker to `zdr_capable` routes, same
    // as an explicit for_zdr open (Alt+Z).
    let zdr_only = for_zdr || crate::state::zdr_active(&state.config, state.meta.as_ref());
    let filtered =
        crate::state::picker_models(&state.models, &state.model_flags, &query_lower, zdr_only);

    let list_height = (area.height as usize).saturating_sub(3).min(8);

    if state.models.is_empty() {
        lines.push(Line::from(Span::styled("  loading…", opts.theme.faint)));
    } else if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching models",
            opts.theme.dim,
        )));
        lines.push(Line::from(Span::styled(
            "  backspace to clear the filter",
            opts.theme.faint,
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
            let flags = state
                .model_flags
                .get(&route_str)
                .copied()
                .unwrap_or_default();
            let mut spans = vec![
                Span::styled(
                    format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
                    opts.theme.accent,
                ),
                Span::styled(
                    route_str,
                    if is_sel {
                        opts.theme.bold
                    } else {
                        opts.theme.fg
                    },
                ),
                Span::raw("  "),
                Span::styled(m.display_name.clone(), opts.theme.dim),
            ];
            if flags.zdr_capable {
                spans.push(Span::styled(" [ZDR]", opts.theme.green));
            }
            if flags.favorite {
                let star = if state.config.ui.ascii_only {
                    "*"
                } else {
                    "\u{2605}"
                };
                spans.push(Span::styled(format!(" {star}"), opts.theme.yellow));
            }
            let line = Line::from(spans);
            if is_sel {
                lines.push(selected_row(line, area.width, opts));
            } else {
                lines.push(line);
            }
        }
    }

    while lines.len() < (area.height as usize).saturating_sub(1) {
        lines.push(Line::default());
    }

    // §9.1: the mark chips are generated from the live keymap — a rebound
    // chord shows its own spelling, an unbound action drops out entirely.
    let sep = opts.glyphs.dot_sep;
    let arrows = if opts.glyphs.ascii { "^/v" } else { "↑/↓" };
    let mut items = vec![format!("{arrows} select"), "enter confirm".to_string()];
    for (action, label) in [
        (crate::binding::Action::ModelPickerFavorite, "fav"),
        (crate::binding::Action::ModelPickerZdr, "zdr"),
        (crate::binding::Action::ModelPickerRefresh, "refresh"),
    ] {
        let chords = state
            .keymap
            .bindings_for(action)
            .iter()
            .map(crate::binding::format_binding)
            .collect::<Vec<_>>()
            .join("/");
        if !chords.is_empty() {
            items.push(format!("{chords} {label}"));
        }
    }
    items.push("esc cancel".to_string());
    let hint = items.join(sep);
    lines.push(Line::from(Span::styled(
        pacode_render::truncate_to_width(&hint, area.width as usize, true),
        opts.theme.dim,
    )));

    frame.render_widget(Paragraph::new(lines), area);

    // Set cursor on query input
    let cursor_x = area.x + 2 + (query.len() as u16).min(area.width.saturating_sub(3));
    frame.set_cursor_position((cursor_x, area.y + 1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_picker_marker_aligns_with_label_center() {
        let effort_labels = ["low", "medium", "high", "max"];
        let mode_labels = ["Build", "Auto", "Plan", "Bypass"];

        for width in [40, 48, 60, 80, 100, 120] {
            let (start, track_w) = picker_track_geometry(width);

            for i in 0..effort_labels.len() {
                let marker_col = option_center_x(start, track_w, i, effort_labels.len());
                let label_start = option_label_start(marker_col, effort_labels[i].len());
                let label_center = option_label_center(label_start, effort_labels[i].len());
                assert_eq!(
                    marker_col, label_center,
                    "Effort index {i} mismatch at width {width}: marker {marker_col} != center {label_center}"
                );
            }

            for i in 0..mode_labels.len() {
                let marker_col = option_center_x(start, track_w, i, mode_labels.len());
                let label_start = option_label_start(marker_col, mode_labels[i].len());
                let label_center = option_label_center(label_start, mode_labels[i].len());
                assert_eq!(
                    marker_col, label_center,
                    "Mode index {i} mismatch at width {width}: marker {marker_col} != center {label_center}"
                );
            }
        }
    }
}
