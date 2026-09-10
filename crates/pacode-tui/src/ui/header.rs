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

/// Welcome cascade stagger: one mascot row arrives per this many milliseconds,
/// then the title, then the byline.
const CASCADE_ROW_MS: u64 = 40;

/// Render the non-persisted header transcript cell.
/// Below width 40 the mascot is dropped.
///
/// `welcome_ms` is the age of the session UI. Inside the welcome window the
/// banner cascades in: mascot rows top-down, one per `CASCADE_ROW_MS`, then the
/// title, then the byline. Rows that have not arrived yet are blank
/// placeholders, so the banner's height — and the transcript's scroll — never
/// shifts while the cascade plays. Past the window this is the same static
/// banner as before.
pub fn render(
    info: &HeaderInfo,
    width: u16,
    opts: &RenderOptions,
    frame: u64,
    welcome_ms: u64,
) -> Vec<Line<'static>> {
    let title = format!("pacode v{}", info.version);

    let stage = if welcome_ms < crate::state::WELCOME_ANIM_MS {
        welcome_ms / CASCADE_ROW_MS
    } else {
        u64::MAX
    };

    if width < 40 {
        return vec![
            if stage > 0 {
                Line::from(Span::styled(title, opts.theme.bold))
            } else {
                Line::default()
            },
            if stage > 1 {
                Line::from(Span::styled(BYLINE, opts.theme.faint))
            } else {
                Line::default()
            },
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
    // The title arrives once every mascot row has landed; the byline one step later.
    let title_due = stage > mascot_rows as u64;
    let byline_due = stage > mascot_rows as u64 + 1;
    let mut lines = Vec::with_capacity(mascot_rows + 2);
    for (row, mascot_line) in mascot_lines.into_iter().enumerate() {
        if row as u64 >= stage {
            lines.push(Line::default());
            continue;
        }
        let drawn = line_width(&mascot_line);
        let mut spans = mascot_line.spans;
        if row == title_row && title_due {
            let pad = mascot_width.saturating_sub(drawn) + GAP;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(title.clone(), opts.theme.bold));
            if let Some(pad) = phrase_pad {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(phrase, opts.theme.faint));
            }
        } else if row == title_row + 1 && byline_due {
            let pad = mascot_width.saturating_sub(drawn) + GAP;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(BYLINE, opts.theme.faint));
        }
        lines.push(Line::from(spans));
    }
    // A mascot too short to hold the byline row still gets the byline, under the block.
    if title_row + 1 >= mascot_rows {
        lines.push(if byline_due {
            Line::from(vec![
                Span::raw(" ".repeat(mascot_width + GAP)),
                Span::styled(BYLINE, opts.theme.faint),
            ])
        } else {
            Line::default()
        });
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
