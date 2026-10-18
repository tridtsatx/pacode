//! Line-range helpers: a small capped set of 0-based, half-open ranges and the
//! overlap rule used to decide whether a write touches someone else's change.

use std::ops::Range;

/// Most intervals kept per (agent, path). Past it the smallest interval is dropped:
/// forgetting part of a read can only cost a re-read, never hide a conflict.
pub const MAX_INTERVALS: usize = 64;

/// Sorted, disjoint, non-adjacent line ranges (adjacent ones are merged).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineRanges {
    spans: Vec<Range<usize>>,
}

impl LineRanges {
    pub fn spans(&self) -> &[Range<usize>] {
        &self.spans
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn clear(&mut self) {
        self.spans.clear();
    }

    /// Total number of lines covered.
    pub fn line_count(&self) -> usize {
        self.spans.iter().map(|r| r.end - r.start).sum()
    }

    /// Add `r`, merging it with overlapping and adjacent intervals. Empty ranges are
    /// ignored.
    pub fn insert(&mut self, r: Range<usize>) {
        if r.start >= r.end {
            return;
        }
        let mut merged = r;
        let mut out: Vec<Range<usize>> = Vec::with_capacity(self.spans.len() + 1);
        let mut placed = false;
        for s in self.spans.drain(..) {
            if s.end < merged.start {
                out.push(s);
            } else if merged.end < s.start {
                if !placed {
                    out.push(merged.clone());
                    placed = true;
                }
                out.push(s);
            } else {
                merged = merged.start.min(s.start)..merged.end.max(s.end);
            }
        }
        if !placed {
            out.push(merged);
        }
        self.spans = out;
        while self.spans.len() > MAX_INTERVALS {
            let smallest = self
                .spans
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.end - s.start)
                .map(|(i, _)| i);
            match smallest {
                Some(i) => {
                    self.spans.remove(i);
                }
                None => break,
            }
        }
    }

    /// Whether every line of `r` is in the set (an empty `r` counts as covered).
    pub fn covers(&self, r: &Range<usize>) -> bool {
        if r.start >= r.end {
            return true;
        }
        self.spans
            .iter()
            .any(|s| s.start <= r.start && r.end <= s.end)
    }
}

/// Whether two line ranges collide. An empty range is a point (an insertion or a
/// deletion between lines); a point collides with a range it lies in or borders, and
/// with an equal point.
pub fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    match (a.is_empty(), b.is_empty()) {
        (false, false) => a.start < b.end && b.start < a.end,
        (true, false) => b.start <= a.start && a.start <= b.end,
        (false, true) => a.start <= b.start && b.start <= a.end,
        (true, true) => a.start == b.start,
    }
}

/// The lines a change occupies in a file of `n` lines: a point shows the line after
/// it (the last line at EOF; nothing in an empty file).
pub fn visible(r: &Range<usize>, n: usize) -> Range<usize> {
    if r.start < r.end || n == 0 {
        return r.clone();
    }
    let p = r.start.min(n - 1);
    p..p + 1
}

/// 1-based, inclusive form of a 0-based range: `L3-L5`, `L7` (or `3-5`, `7` without
/// the `L` prefix).
pub fn describe(r: &Range<usize>, prefix: &str) -> String {
    let first = r.start + 1;
    let last = r.end.max(r.start + 1);
    if last == first {
        format!("{prefix}{first}")
    } else {
        format!("{prefix}{first}-{prefix}{last}")
    }
}
