//! Diffs for the edit tools and the transcript.

use pacode_types::DiffStat;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::RenderOptions;
use crate::wrap::truncate_to_width;

#[cfg(test)]
#[path = "diff_tests.rs"]
mod diff_tests;

/// Added/removed line counts between two texts.
pub fn diff_stat(old: &str, new: &str) -> DiffStat {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0u32;
    let mut removed = 0u32;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    DiffStat { added, removed }
}

/// Unified diff text with `context` lines and `a/<path>` / `b/<path>` headers.
pub fn unified_diff(old: &str, new: &str, path: &str, context: usize) -> String {
    let a_header = format!("a/{path}");
    let b_header = format!("b/{path}");
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context)
        .header(&a_header, &b_header)
        .to_string()
}

/// Colour a unified diff: `+` green, `-` red, `@@` dim, headers faint. Lines longer than
/// the width are truncated with `…` (diffs are not wrapped).
pub fn render_diff(diff: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for line in diff.lines() {
        let style =
            if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff") {
                opts.theme.faint
            } else if line.starts_with('+') {
                opts.theme.green
            } else if line.starts_with('-') {
                opts.theme.red
            } else if line.starts_with("@@") {
                opts.theme.dim
            } else {
                opts.theme.fg
            };

        let truncated = truncate_to_width(line, opts.width as usize, true);
        lines.push(Line::from(Span::styled(truncated, style)));
    }
    lines
}
