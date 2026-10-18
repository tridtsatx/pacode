//! Rendering of the "stale anchors" rejection message.

use std::ops::Range;

use super::anchor::StaleAnchor;
use super::render::snippet;
use super::{CONTEXT_LINES, MAX_CONTEXT_LINES};

/// A stale anchor plus the 0-based line it pointed at (for context).
pub(crate) struct StaleHit {
    pub info: StaleAnchor,
    pub focus: usize,
}

pub(crate) fn stale_message(content: &str, hits: &[StaleHit]) -> String {
    let mut out = format!(
        "Edit rejected, nothing written: {} stale anchor(s); the file changed since it was read.\n",
        hits.len()
    );
    for h in hits {
        out.push_str(&format!(
            "- op {} {} `{}`: {}\n",
            h.info.op, h.info.role, h.info.anchor, h.info.reason
        ));
    }
    let focus: Vec<Range<usize>> = hits.iter().map(|h| h.focus..h.focus + 1).collect();
    out.push_str("Current lines (`>` = stale anchor position); retry using these anchors:\n");
    out.push_str(&snippet(
        content,
        &focus,
        CONTEXT_LINES,
        MAX_CONTEXT_LINES,
        true,
    ));
    out
}
