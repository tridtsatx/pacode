//! Markdown → styled, wrapped ratatui lines. Port the structure of jcode's
//! `jcode-render-core` (pulldown-cmark event walk into a block model, then layout),
//! without syntax highlighting or images.
//!
//! Supported: paragraphs, headings (bold, accent), emphasis/strong, inline code
//! (accent), fenced code blocks (dim `│` gutter, no highlighting), bullet and numbered
//! lists (nested, `•`/`1.`), block quotes (dim bar), horizontal rule, links (text +
//! dim URL), tables (simple column alignment, rendered only when complete), soft/hard
//! breaks. Unknown constructs degrade to plain text; never drop content.

use ratatui::text::Line;

use crate::RenderOptions;

/// Render `source` into wrapped lines for `opts.width`.
pub fn render_markdown(source: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    let _ = (source, opts);
    todo!("markdown::render_markdown")
}

/// Split `source` into a stable prefix (complete blocks) and a mutable tail (the last
/// block, which may still be streaming). Used by the streaming cell so that only the
/// tail is re-rendered on each delta. A fenced code block or table that is not closed
/// makes the whole block part of the tail.
pub fn split_stable_tail(source: &str) -> (&str, &str) {
    let _ = source;
    todo!("markdown::split_stable_tail")
}
