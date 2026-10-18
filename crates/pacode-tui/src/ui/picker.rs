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
            draw_question(
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

fn effort_description(effort: pacode_types::Effort) -> &'static str {
    use pacode_types::Effort;
    match effort {
        Effort::Low => "low: fastest, minimal reasoning",
        Effort::Medium => "medium: balanced",
        Effort::High => "high: deep reasoning for hard tasks",
        Effort::XHigh => "xhigh: extra reasoning; sent as `high` unless the provider maps it",
        Effort::Max => "max: maximum reasoning, slow and expensive",
    }
}

fn draw_effort(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: usize,
    opts: &RenderOptions,
) {
    // Only the levels the current model supports.
    let efforts = state.effort_levels();
    let levels: Vec<&str> = efforts.iter().map(|e| e.as_str()).collect();
    let descriptions: Vec<&str> = efforts.iter().map(|e| effort_description(*e)).collect();

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
        let style = if is_sel {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        opt_spans.push(Span::styled(lvl, style));
        cur_col = label_start + lvl.len();
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

/// The question the model asked, as a bottom picker: the header chip, the
/// question, one row per option with its description, and the key hints.
#[allow(clippy::too_many_arguments)]
fn draw_question(
    frame: &mut Frame,
    area: Rect,
    question: &pacode_types::Question,
    index: usize,
    selected: &[usize],
    typed: &str,
    typing: bool,
    opts: &RenderOptions,
) {
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    if !question.header.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {} ", question.header),
            opts.theme.selected_bg,
        )));
    }
    for line in pacode_render::wrap_text(&question.question, width) {
        lines.push(Line::from(Span::styled(line, opts.theme.bold)));
    }

    for (i, option) in question.options.iter().enumerate() {
        let is_cursor = i == index;
        let is_selected = selected.contains(&i);
        let marker = if is_selected {
            opts.glyphs.ok
        } else if is_cursor {
            opts.glyphs.pointer
        } else {
            " "
        };
        let marker_style = if is_selected {
            opts.theme.green
        } else {
            opts.theme.accent
        };
        let mut spans = vec![
            Span::styled(format!("{marker} "), marker_style),
            Span::styled(
                format!("{}. {}", i + 1, option.label),
                if is_cursor {
                    opts.theme.bold
                } else {
                    opts.theme.fg
                },
            ),
        ];
        if option.recommended {
            spans.push(Span::styled(" (recommended)", opts.theme.faint));
        }
        let line = Line::from(spans);
        if is_cursor {
            lines.push(selected_row(line, width as u16, opts));
        } else {
            lines.push(line);
        }
        if is_cursor && !option.description.is_empty() {
            for dl in pacode_render::wrap_text(&option.description, width.saturating_sub(4)) {
                lines.push(selected_row(
                    Line::from(Span::styled(format!("    {dl}"), opts.theme.dim)),
                    width as u16,
                    opts,
                ));
            }
        }
    }

    let hint = if typing {
        format!("type an answer · enter send · esc back    {typed}")
    } else if question.multi_select {
        "space toggle · enter send · 1-9 pick one · t type · esc dismiss".to_string()
    } else {
        "enter select · 1-9 pick · t type an answer · esc dismiss".to_string()
    };
    lines.push(Line::from(Span::styled(
        pacode_render::truncate_to_width(&hint, width, true),
        opts.theme.dim,
    )));

    frame.render_widget(Paragraph::new(lines), area);
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
