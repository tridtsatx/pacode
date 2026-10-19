//! The status background of a tool block (omp's state-tinted tool card): the
//! whole block — header and body rows — sits on one band whose tint says how
//! the call went. The blank row that separates cells stays unpainted.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, display_width};
use pacode_types::transcript::ToolStatus;

#[cfg(test)]
#[path = "toolcell_bg_tests.rs"]
mod toolcell_bg_tests;

/// Band for a call's outcome. Running is pending; a failure or a refusal is
/// an error. A call cancelled or interrupted mid-turn arrives as `Error`
/// (the wire has no separate status for it), so it reads as one too. A call
/// moved to the background did what it was asked — start the job — and the
/// job's own result cell later carries its outcome.
pub(crate) fn status_bg(status: ToolStatus, opts: &RenderOptions) -> Style {
    match status {
        ToolStatus::Running => opts.theme.tool_pending_bg,
        ToolStatus::Ok | ToolStatus::Backgrounded => opts.theme.tool_success_bg,
        ToolStatus::Error | ToolStatus::Denied => opts.theme.tool_error_bg,
    }
}

/// Paint `bg` across every row of a block: a span without a background of
/// its own takes the band (a badge or a diff band keeps its own), and each
/// row is padded to `width` so the band runs edge to edge. An empty `bg`
/// (ANSI-16, mono) leaves the rows exactly as they were.
pub(crate) fn paint_block(lines: &mut [Line<'static>], bg: Style, width: usize) {
    let Some(color) = bg.bg else {
        return;
    };
    for line in lines {
        for span in &mut line.spans {
            if matches!(span.style.bg, None | Some(Color::Reset)) {
                span.style.bg = Some(color);
            }
        }
        let used: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
        if used < width {
            line.spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(color),
            ));
        }
    }
}
