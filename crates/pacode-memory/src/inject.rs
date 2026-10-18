//! System-prompt memory section: pinned entries in full plus a one-line index of the
//! most recent others. Deterministic (pinned: oldest first, index: newest first) so the
//! prompt prefix stays byte-stable while memory is unchanged. Pure rendering; the store
//! produces the [`Snapshot`].

use crate::text::{collapse_ws, truncate_chars};
use crate::types::Entry;

/// Pinned lines injected per scope.
pub const PINNED_CAP: usize = 20;
/// Index lines (non-pinned) injected per scope.
pub const RECENT_CAP: usize = 10;
const PINNED_LINE_CHARS: usize = 300;

pub const GUIDANCE: &str = "Entries marked with an id are stored memories; the Recent list is an index \
(title + id) only. Call memory_recall(query) for details, memory_write when you learn durable \
facts or user preferences, and memory_forget(id) for stale ones.";

#[derive(Clone, Debug, Default)]
pub struct ScopeSnapshot {
    pub pinned: Vec<Entry>,
    pub recent: Vec<Entry>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub global: ScopeSnapshot,
    pub project: ScopeSnapshot,
}

fn pinned_line(e: &Entry) -> String {
    let text = collapse_ws(&e.text);
    let body = if text.starts_with(&e.title) {
        text
    } else {
        format!("{}: {text}", e.title)
    };
    let capped = truncate_chars(&body, PINNED_LINE_CHARS);
    let ellipsis = if capped.len() < body.len() { "…" } else { "" };
    format!("- {capped}{ellipsis} ({})", e.id)
}

fn index_line(e: &Entry) -> String {
    format!("- {} ({})", collapse_ws(&e.title), e.id)
}

struct Line {
    scope: usize,
    recent: bool,
    text: String,
}

fn compose(lines: &[Line], keep: &[bool], omitted: usize) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for (scope, name) in ["global", "project"].iter().enumerate() {
        let pick = |recent: bool| -> Vec<&str> {
            lines
                .iter()
                .zip(keep)
                .filter(|(l, k)| **k && l.scope == scope && l.recent == recent)
                .map(|(l, _)| l.text.as_str())
                .collect()
        };
        let (pinned, recent) = (pick(false), pick(true));
        if pinned.is_empty() && recent.is_empty() {
            continue;
        }
        let mut b = format!("# Memory ({name})");
        if !pinned.is_empty() {
            b.push_str("\n\n");
            b.push_str(&pinned.join("\n"));
        }
        if !recent.is_empty() {
            b.push_str("\n\nRecent (index only):\n");
            b.push_str(&recent.join("\n"));
        }
        blocks.push(b);
    }
    if omitted > 0 {
        blocks.push(format!(
            "[... {omitted} more memories omitted; use memory_recall ...]"
        ));
    }
    blocks.push(GUIDANCE.to_string());
    blocks.join("\n\n")
}

/// Render the section within `cap_chars` (characters). Over the cap, index lines are
/// dropped first (last scope first), then pinned lines, then the text is hard-cut.
/// `None` when there is nothing to show or the cap is 0.
pub fn render(snap: &Snapshot, cap_chars: usize) -> Option<String> {
    if cap_chars == 0 {
        return None;
    }
    let mut lines: Vec<Line> = Vec::new();
    for (scope, s) in [&snap.global, &snap.project].into_iter().enumerate() {
        for e in s.pinned.iter().take(PINNED_CAP) {
            lines.push(Line {
                scope,
                recent: false,
                text: pinned_line(e),
            });
        }
        for e in s.recent.iter().take(RECENT_CAP) {
            lines.push(Line {
                scope,
                recent: true,
                text: index_line(e),
            });
        }
    }
    if lines.is_empty() {
        return None;
    }
    let mut drop_order: Vec<usize> = (0..lines.len()).filter(|i| lines[*i].recent).collect();
    drop_order.reverse();
    let mut pinned_idx: Vec<usize> = (0..lines.len()).filter(|i| !lines[*i].recent).collect();
    pinned_idx.reverse();
    drop_order.extend(pinned_idx);

    let mut keep = vec![true; lines.len()];
    let mut text = compose(&lines, &keep, 0);
    for (n, idx) in drop_order.iter().enumerate() {
        if text.chars().count() <= cap_chars {
            return Some(text);
        }
        keep[*idx] = false;
        text = compose(&lines, &keep, n + 1);
    }
    Some(truncate_chars(&text, cap_chars))
}
