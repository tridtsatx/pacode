//! Header cell rendered at the top of the transcript (spec §9).

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use pacode_render::RenderOptions;

use crate::state::transcript::HeaderInfo;

#[cfg(test)]
#[path = "header_tests.rs"]
mod header_tests;

/// Render the non-persisted header transcript cell.
/// Below width 40 the mascot is dropped.
pub fn render(info: &HeaderInfo, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let yellow = Style::default().fg(Color::Yellow);
    let dot = opts.glyphs.dot_sep;

    let line1_text = format!("pacode v{}", info.version);
    let line2_text = format!(
        "{} with {} effort{dot}{}",
        info.model, info.effort, info.provider
    );
    let line3_text = info.cwd.clone();
    let hint_text = format!(
        "Using {} (from {}){dot}/model",
        info.model, info.config_path
    );

    if width < 40 {
        return vec![
            Line::from(Span::styled(line1_text, opts.theme.bold)),
            Line::from(Span::styled(line2_text, opts.theme.dim)),
            Line::from(Span::styled(line3_text, opts.theme.dim)),
            Line::default(),
            Line::from(Span::styled(hint_text, opts.theme.dim)),
            Line::default(),
        ];
    }

    if opts.glyphs.ascii {
        vec![
            Line::from(vec![
                Span::styled("(C  ", yellow),
                Span::styled(line1_text, opts.theme.bold),
            ]),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(line2_text, opts.theme.dim),
            ]),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(line3_text, opts.theme.dim),
            ]),
            Line::default(),
            Line::from(Span::styled(hint_text, opts.theme.dim)),
            Line::default(),
        ]
    } else {
        vec![
            Line::from(vec![
                Span::styled(" ▄▄▄▄▄     ", yellow),
                Span::styled(line1_text, opts.theme.bold),
            ]),
            Line::from(vec![
                Span::styled("█ ▀ ██     ", yellow),
                Span::styled(line2_text, opts.theme.dim),
            ]),
            Line::from(vec![
                Span::styled("███▀       ", yellow),
                Span::styled(line3_text, opts.theme.dim),
            ]),
            Line::from(vec![Span::styled(" ▀▀▀▀▀", yellow)]),
            Line::default(),
            Line::from(Span::styled(hint_text, opts.theme.dim)),
            Line::default(),
        ]
    }
}
