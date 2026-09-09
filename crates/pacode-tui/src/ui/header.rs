//! Header cell rendered at the top of the transcript (spec §9): the mascot plus
//! the app name and version, nothing else.

use ratatui::text::{Line, Span};

use pacode_render::RenderOptions;

use crate::state::transcript::HeaderInfo;
use crate::ui::mascot;

#[cfg(test)]
#[path = "header_tests.rs"]
mod header_tests;

/// Gap between the mascot and the title.
const GAP: usize = 3;

/// Render the non-persisted header transcript cell.
/// Below width 40 the mascot is dropped.
pub fn render(info: &HeaderInfo, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let title = format!("pacode v{}", info.version);

    if width < 40 {
        return vec![
            Line::from(Span::styled(title, opts.theme.bold)),
            Line::default(),
        ];
    }

    let mascot_lines = if opts.glyphs.ascii {
        mascot::render_ascii()
    } else {
        mascot::render(info.mascot)
    };

    // Vertically centre the single title line against the mascot block.
    let title_row = mascot_lines.len().saturating_sub(1) / 2;
    let mascot_width = if opts.glyphs.ascii {
        mascot_lines.iter().map(line_width).max().unwrap_or(0)
    } else {
        mascot::WIDTH
    };

    let mut lines = Vec::with_capacity(mascot_lines.len() + 1);
    for (row, mascot_line) in mascot_lines.into_iter().enumerate() {
        let drawn = line_width(&mascot_line);
        let mut spans = mascot_line.spans;
        if row == title_row {
            let pad = mascot_width.saturating_sub(drawn) + GAP;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(title.clone(), opts.theme.bold));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());

    lines
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum()
}
