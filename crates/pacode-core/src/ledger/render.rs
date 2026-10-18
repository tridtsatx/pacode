//! Model-facing ledger messages: rejections, notes and the compaction digest.

use std::fmt::Write as _;
use std::ops::Range;

use pacode_tools::WriteIntent;
use pacode_tools::hashline::{MAX_CONTEXT_LINES, snippet};
use pacode_types::AgentId;

use super::ranges::{LineRanges, describe, visible};
use super::state::{DigestRow, Drift};

/// Context lines shown (and counted as re-read) around each changed region.
const CONTEXT: usize = 2;

/// Cap on the lines of the compaction digest, header included.
pub const DIGEST_MAX_LINES: usize = 20;

/// Rows of the digest: [`DIGEST_MAX_LINES`] minus header, hint and "omitted" lines.
pub const DIGEST_MAX_ROWS: usize = DIGEST_MAX_LINES - 3;

pub fn not_read(shown: &str) -> String {
    format!(
        "read {shown} first: the file exists but you have not read it (or your read state \
         for it was dropped), and other agents may be editing it. Read it, then retry."
    )
}

fn who(drift: &Drift, viewer: &AgentId) -> String {
    drift
        .writers
        .iter()
        .map(|w| w.describe(viewer))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Current-line regions of the drift (points widened to the line after them).
fn regions(drift: &Drift) -> Vec<Range<usize>> {
    drift
        .changes
        .iter()
        .map(|c| visible(&c.new, drift.line_count))
        .collect()
}

fn list(regions: &[Range<usize>], prefix: &str) -> String {
    let parts: Vec<String> = regions
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| describe(r, prefix))
        .collect();
    if parts.is_empty() {
        format!("{prefix}1")
    } else {
        parts.join(", ")
    }
}

/// `(n lines changed at L3-L5, L40)` parts: (count, `L` list, plain list).
fn summary(drift: &Drift) -> (usize, String, String) {
    let regions = regions(drift);
    let n = drift.changes.iter().map(|c| c.size()).sum();
    (n, list(&regions, "L"), list(&regions, ""))
}

/// The rejection for a write that overlaps changes the agent has not seen, and the
/// current-line ranges it shows with fresh anchors (empty when too many to show);
/// the caller counts those as re-read so one retry is enough.
pub fn conflict(
    shown: &str,
    drift: &Drift,
    viewer: &AgentId,
    content: &str,
    intent: WriteIntent,
) -> (String, Vec<Range<usize>>) {
    let (n, at, plain) = summary(drift);
    let mut msg = format!(
        "{shown} was modified by {} ({n} lines changed at {at}) after you read it. Read \
         {shown} lines {plain} again before writing.",
        who(drift, viewer)
    );
    match intent {
        WriteIntent::Write => msg.push_str(
            " A full `write` must not revert those lines; prefer `edit`, which only checks \
             the lines you change.",
        ),
        WriteIntent::Edit | WriteIntent::Hashline => {}
    }
    let regions = regions(drift);
    let mut windows = LineRanges::default();
    for r in &regions {
        windows.insert(r.start.saturating_sub(CONTEXT)..(r.end + CONTEXT).min(drift.line_count));
    }
    let rows = windows.line_count() + windows.spans().len();
    if windows.is_empty() || rows > MAX_CONTEXT_LINES {
        msg.push_str(
            "\nToo many lines changed to show them here: read those lines with `read` \
             (offset/limit), then retry.",
        );
        return (msg, Vec::new());
    }
    let _ = write!(
        msg,
        "\nCurrent lines (`>` = changed; anchors are fresh and these lines now count as \
         read, so you can retry right away):\n{}",
        snippet(content, &regions, CONTEXT, MAX_CONTEXT_LINES, true)
    );
    (msg, windows.spans().to_vec())
}

/// Appended to a successful write when the file changed elsewhere since the read.
pub fn disjoint_note(shown: &str, drift: &Drift, viewer: &AgentId) -> String {
    let (n, at, plain) = summary(drift);
    format!(
        "Note: {shown} was also modified by {} since you read it ({n} lines changed at \
         {at}); your change did not touch those lines and was applied. Re-read {shown} \
         lines {plain} only if your change depends on them.",
        who(drift, viewer)
    )
}

/// The compaction pin for one agent; `None` when it has seen no files.
pub fn digest(rows: &[(String, DigestRow)], omitted: usize, viewer: &AgentId) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Files you read or wrote (ledger, verbatim)\nOther agents may be editing these \
         too. Earlier `read` output was compacted away: re-read before a hashline edit. \
         Files not listed must be read before you change them.\n",
    );
    for (shown, row) in rows {
        let status = match &row.drift {
            Some(drift) => {
                let (n, at, plain) = summary(drift);
                format!(
                    "MODIFIED by {} after your read ({n} lines at {at}): read lines {plain} \
                     again before writing",
                    who(drift, viewer)
                )
            }
            None if row.wrote => "last written by you; in sync".to_string(),
            None if row.read.is_empty() => "in sync".to_string(),
            None => {
                let spans = row.read.spans();
                let mut ranges: Vec<String> =
                    spans.iter().take(4).map(|r| describe(r, "L")).collect();
                if spans.len() > 4 {
                    ranges.push("...".to_string());
                }
                format!("read {}; in sync", ranges.join(", "))
            }
        };
        let _ = writeln!(out, "- {shown} @{}: {status}", row.version);
    }
    if omitted > 0 {
        let _ = writeln!(out, "- ({omitted} older files omitted)");
    }
    Some(out.trim_end().to_string())
}
