//! Diffs for the edit tools and the transcript.

use codeapp_types::DiffStat;
use ratatui::text::Line;

use crate::RenderOptions;

/// Added/removed line counts between two texts.
pub fn diff_stat(old: &str, new: &str) -> DiffStat {
    let _ = (old, new);
    todo!("diff::diff_stat")
}

/// Unified diff text with `context` lines and `a/<path>` / `b/<path>` headers.
pub fn unified_diff(old: &str, new: &str, path: &str, context: usize) -> String {
    let _ = (old, new, path, context);
    todo!("diff::unified_diff")
}

/// Colour a unified diff: `+` green, `-` red, `@@` dim, headers faint. Lines longer than
/// the width are truncated with `…` (diffs are not wrapped).
pub fn render_diff(diff: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    let _ = (diff, opts);
    todo!("diff::render_diff")
}
