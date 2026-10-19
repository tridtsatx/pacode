//! Which lines of a new text were ADDED relative to an old one.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use similar::{ChangeTag, TextDiff};

/// Above this many (old lines x new lines) the exact diff is replaced by the
/// set-based approximation to bound CPU.
const EXACT_DIFF_BUDGET: usize = 4_000_000;

/// Wall-clock cap on the exact diff; past it `similar` returns a valid but
/// approximate (less minimal) script.
const EXACT_DIFF_DEADLINE: Duration = Duration::from_millis(20);

/// 0-based indexes of the lines of `new` that are inserted relative to `old`
/// (changed lines count as added). `old = None` means a new file: every line.
pub fn added_line_indices(old: Option<&str>, new: &str) -> Vec<usize> {
    let line_count = new.lines().count();
    let Some(old) = old else {
        return (0..line_count).collect();
    };
    if old.lines().count().saturating_mul(line_count) > EXACT_DIFF_BUDGET {
        return added_by_set(&OldLines::new(old), new);
    }
    let diff = TextDiff::configure()
        .deadline(Instant::now() + EXACT_DIFF_DEADLINE)
        .diff_lines(old, new);
    let mut added = Vec::new();
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Insert
            && let Some(idx) = change.new_index()
        {
            added.push(idx);
        }
    }
    added
}

/// Trimmed lines of the previous file content, built once per tool call and
/// reused for repeated scans of a growing text (streaming). Owner: the caller;
/// dropped with the call. Size: bounded by the old file (callers cap it at 2 MB).
#[derive(Clone, Debug, Default)]
pub struct OldLines(HashSet<String>);

impl OldLines {
    pub fn new(old: &str) -> Self {
        OldLines(old.lines().map(|l| l.trim().to_string()).collect())
    }

    pub fn contains(&self, line: &str) -> bool {
        self.0.contains(line.trim())
    }
}

/// Cheap approximation: a line is added when no identical (trimmed) line exists in
/// the old text. Blank lines are never added.
pub fn added_by_set(old: &OldLines, new: &str) -> Vec<usize> {
    new.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !old.contains(l))
        .map(|(i, _)| i)
        .collect()
}
