//! Header cell rendered at the top of the transcript (spec §9): the mascot, the
//! app name and version, the byline, and the phrase of the day on the right.

use ratatui::text::{Line, Span};

use pacode_render::RenderOptions;

use crate::state::transcript::HeaderInfo;
use crate::ui::{mascot, phrases};

#[cfg(test)]
#[path = "header_tests.rs"]
mod header_tests;

/// Gap between the mascot and the title.
const GAP: usize = 3;

/// Byline under the title.
const BYLINE: &str = "made by tridtsat";

/// Minimum gap between the title block and the right-aligned phrase of the day;
/// below it the phrase is dropped rather than crowding the title.
const PHRASE_GAP: usize = 4;

/// Render the non-persisted header transcript cell.
/// Below width 40 the mascot is dropped.
pub fn render(
    info: &HeaderInfo,
    width: u16,
    opts: &RenderOptions,
    frame: u64,
) -> Vec<Line<'static>> {
    let title = format!("pacode v{}", info.version);

    if width < 40 {
        return vec![
            Line::from(Span::styled(title, opts.theme.bold)),
            Line::from(Span::styled(BYLINE, opts.theme.faint)),
            Line::default(),
        ];
    }

    let mascot_lines = if opts.glyphs.ascii {
        mascot::render_ascii_frame(frame, info.truecolor)
    } else {
        mascot::render_frame(info.mascot, frame, info.truecolor)
    };

    // Vertically centre the single title line against the mascot block.
    let title_row = mascot_lines.len().saturating_sub(1) / 2;
    let mascot_width = if opts.glyphs.ascii {
        mascot_lines.iter().map(line_width).max().unwrap_or(0)
    } else {
        mascot::WIDTH
    };

    let phrase = phrases::phrase_of_the_day(info.day);
    let title_end = mascot_width + GAP + pacode_render::display_width(&title);
    let phrase_w = pacode_render::display_width(phrase);
    // Right-aligned on the title row, only when it fits without crowding the title.
    let phrase_pad = (width as usize)
        .checked_sub(title_end + PHRASE_GAP + phrase_w)
        .map(|slack| slack + PHRASE_GAP);

    let mascot_rows = mascot_lines.len();
    let mut lines = Vec::with_capacity(mascot_rows + 2);
    for (row, mascot_line) in mascot_lines.into_iter().enumerate() {
        let drawn = line_width(&mascot_line);
        let mut spans = mascot_line.spans;
        if row == title_row {
            let pad = mascot_width.saturating_sub(drawn) + GAP;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(title.clone(), opts.theme.bold));
            if let Some(pad) = phrase_pad {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(phrase, opts.theme.faint));
            }
        } else if row == title_row + 1 {
            let pad = mascot_width.saturating_sub(drawn) + GAP;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(BYLINE, opts.theme.faint));
        }
        lines.push(Line::from(spans));
    }
    // A mascot too short to hold the byline row still gets the byline, under the block.
    if title_row + 1 >= mascot_rows {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(mascot_width + GAP)),
            Span::styled(BYLINE, opts.theme.faint),
        ]));
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
