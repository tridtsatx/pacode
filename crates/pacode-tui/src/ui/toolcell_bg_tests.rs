//! The status band of a tool block: one tint per outcome on every row, edge
//! to edge, nothing under ANSI-16 or mono.

use ratatui::style::Style;
use ratatui::text::Line;

use pacode_render::{ColorMode, RenderOptions, Theme, display_width};
use pacode_types::transcript::ToolStatus;

use super::{paint_block, status_bg};
use crate::ui::toolcell::{ToolCallView, render};

fn truecolor(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn ansi(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::ansi(), ColorMode::Ansi)
}

fn call(status: ToolStatus) -> ToolCallView<'static> {
    ToolCallView {
        name: "bash",
        title: "bash cargo test",
        status,
        preview: "running 3 tests\ntest a ... ok\n[exit code 0]",
        diff: None,
        duration_ms: Some(1200),
        streamed: None,
    }
}

fn width_of(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| display_width(&s.content)).sum()
}

const STATUSES: [ToolStatus; 5] = [
    ToolStatus::Running,
    ToolStatus::Ok,
    ToolStatus::Error,
    ToolStatus::Denied,
    ToolStatus::Backgrounded,
];

#[test]
fn each_outcome_has_its_band() {
    let opts = truecolor(60);
    let theme = &opts.theme;
    assert_eq!(status_bg(ToolStatus::Running, &opts), theme.tool_pending_bg);
    assert_eq!(status_bg(ToolStatus::Ok, &opts), theme.tool_success_bg);
    assert_eq!(status_bg(ToolStatus::Error, &opts), theme.tool_error_bg);
    assert_eq!(status_bg(ToolStatus::Denied, &opts), theme.tool_error_bg);
    assert_eq!(
        status_bg(ToolStatus::Backgrounded, &opts),
        theme.tool_success_bg
    );
    // The three bands are three different colours.
    assert!(theme.tool_pending_bg.bg.is_some());
    assert_ne!(theme.tool_pending_bg, theme.tool_success_bg);
    assert_ne!(theme.tool_success_bg, theme.tool_error_bg);
}

#[test]
fn every_row_of_a_tool_block_sits_on_its_band_edge_to_edge() {
    let opts = truecolor(60);
    for status in STATUSES {
        for expanded in [false, true] {
            let lines = render(&call(status), 60, &opts, 0, expanded);
            let band = status_bg(status, &opts).bg;
            let (gap, block) = lines.split_last().expect("a block and its gap");
            assert!(block.len() >= 2, "{status:?}: header and body");
            for row in block {
                assert_eq!(width_of(row), 60, "{status:?}: the band spans the row");
                for span in &row.spans {
                    assert!(span.style.bg.is_some(), "{status:?}: no hole in the band");
                }
                // The badge keeps its own plaque; everything else is the band.
                for span in row.spans.iter().skip(usize::from(row == &block[0])) {
                    assert_eq!(span.style.bg, band, "{status:?}: {:?}", span.content);
                }
            }
            assert_eq!(*gap, Line::default(), "the gap between cells stays bare");
        }
    }
}

#[test]
fn the_badge_keeps_its_own_background() {
    let opts = truecolor(60);
    let lines = render(&call(ToolStatus::Ok), 60, &opts, 0, false);
    assert_eq!(lines[0].spans[0].style, opts.theme.badge(opts.theme.green));
}

#[test]
fn ansi_and_mono_draw_no_band_at_all() {
    for opts in [
        ansi(60),
        RenderOptions::with_theme_color(60, false, Theme::truecolor(), ColorMode::Mono),
    ] {
        for status in STATUSES {
            let lines = render(&call(status), 60, &opts, 0, false);
            for row in &lines[1..] {
                for span in &row.spans {
                    assert_eq!(span.style.bg, None, "{status:?}: {:?}", span.content);
                }
            }
            // Body rows keep their own width: no padding without a band.
            assert!(width_of(&lines[1]) < 60, "{status:?}");
        }
    }
}

#[test]
fn painting_patches_only_spans_without_a_background() {
    let opts = truecolor(20);
    let own = Style::default().bg(ratatui::style::Color::Rgb(1, 2, 3));
    let mut lines = vec![Line::from(vec![
        ratatui::text::Span::styled("ab", own),
        ratatui::text::Span::styled("cd", opts.theme.dim),
    ])];
    let band = opts.theme.tool_error_bg;
    paint_block(&mut lines, band, 20);
    assert_eq!(lines[0].spans[0].style, own);
    assert_eq!(lines[0].spans[1].style, opts.theme.dim.patch(band));
    assert_eq!(width_of(&lines[0]), 20);
    assert_eq!(lines[0].spans[2].style.bg, band.bg);

    // An empty band is a no-op.
    let before = lines.clone();
    paint_block(&mut lines, Style::default(), 40);
    assert_eq!(lines, before);
}
