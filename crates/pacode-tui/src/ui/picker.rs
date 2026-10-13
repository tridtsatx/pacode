//! In-place selectors replacing the input area and footer (bottom of dialog column).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    crate::ui::overlays::clear_panel(frame, area, opts);

    match &state.focus {
        Focus::Overlay(Overlay::EffortPicker { index }) => {
            draw_effort(frame, area, state, *index, opts);
        }
        Focus::Overlay(Overlay::ModePicker { index }) => {
            draw_mode(frame, area, state, *index, opts);
        }
        Focus::Overlay(Overlay::QuestionPicker) => {
            if let Some(question) = state.questions.current() {
                let focus = state.questions.focus().cloned().unwrap_or_default();
                crate::ui::question_picker::draw(
                    frame,
                    area,
                    question,
                    &focus,
                    state.questions.position(),
                    state.questions.len(),
                    opts,
                );
            }
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
