//! Commit-graph lane layout (spec §5.10): one column per active branch line.
//!
//! `git log` rows arrive newest-first; every row maps to one [`LaneCell`] of
//! up to [`MAX_LANES`] columns plus an overflow flag (rendered `»`/`>`).
//!
//! Marks and their glyphs (unicode / `ascii`):
//! - [`LaneMark::Node`] `●`/`*` — the row's commit;
//! - [`LaneMark::Line`] `│`/`|` — a lane continuing downward;
//! - [`LaneMark::Merge`] `╯`/`/` — a lane closing into the row's node;
//! - [`LaneMark::Fork`] `╰`/`\` — a new lane opening for an extra parent;
//! - [`LaneMark::Junction`] `├`/`+` — a continuing lane an edge connects to or
//!   crosses (extra parent already awaited there, or the horizontal part of a
//!   merge/fork edge passes through);
//! - [`LaneMark::Blank`] ` ` — nothing in this column.
//!
//! The tracker is order-tolerant but assumes each hash appears once; callers
//! paging `git log` must dedup appended rows by hash.

use std::collections::HashSet;

use pacode_types::vcs::CommitRow;

/// Columns shown before the graph folds into `»`.
pub const MAX_LANES: usize = 6;

/// One column of a commit row's graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneMark {
    /// Nothing.
    Blank,
    /// The row's commit node.
    Node,
    /// A lane continuing downward.
    Line,
    /// A continuing lane an edge connects to or passes through.
    Junction,
    /// A lane closing into this row's node (it was waiting for this commit).
    Merge,
    /// A lane opening here for one of this commit's extra parents.
    Fork,
}

impl LaneMark {
    /// The glyph for this mark; `ascii` picks the `* | + / \` fallback set.
    pub fn glyph(self, ascii: bool) -> char {
        if ascii {
            match self {
                Self::Blank => ' ',
                Self::Node => '*',
                Self::Line => '|',
                Self::Junction => '+',
                Self::Merge => '/',
                Self::Fork => '\\',
            }
        } else {
            match self {
                Self::Blank => ' ',
                Self::Node => '●',
                Self::Line => '│',
                Self::Junction => '├',
                Self::Merge => '╯',
                Self::Fork => '╰',
            }
        }
    }
}

/// The rendered graph column of one commit row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneCell {
    /// Marks left to right; normally ≤ [`MAX_LANES`].
    pub marks: Vec<LaneMark>,
    /// The history needed more than [`MAX_LANES`] columns at this row.
    pub overflow: bool,
}

impl LaneCell {
    /// Render to a string for the graph column, right-trimmed.
    pub fn line(&self, ascii: bool) -> String {
        let mut s: String = self
            .marks
            .iter()
            .take(MAX_LANES)
            .map(|m| m.glyph(ascii))
            .collect();
        if self.overflow {
            s.push(if ascii { '>' } else { '»' });
        }
        s.trim_end().to_string()
    }
}

/// Lay out one [`LaneCell`] per input row, in input order.
pub fn lanes(rows: &[CommitRow]) -> Vec<LaneCell> {
    // Open edges: lane i is waiting for commit `lanes[i]` (None = free).
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::with_capacity(rows.len());
    let mut cells = Vec::with_capacity(rows.len());

    for row in rows {
        let hash = row.hash.as_str();
        let mut overflow = false;

        // Columns waiting for this commit; the node takes the leftmost one.
        let first_expecting = lanes.iter().position(|l| *l == Some(hash));
        let col = match first_expecting {
            Some(c) => c,
            None => match lanes.iter().position(|l| l.is_none()) {
                Some(c) => c,
                None if lanes.len() < MAX_LANES => {
                    lanes.push(None);
                    lanes.len() - 1
                }
                None => {
                    // All lanes busy: draw the node on a virtual column that
                    // the renderer clips, and track nothing for its parents.
                    lanes.push(None);
                    overflow = true;
                    lanes.len() - 1
                }
            },
        };

        let mut marks = vec![LaneMark::Blank; lanes.len()];
        // Closing lanes: every other column that was waiting for this commit.
        let mut closing = HashSet::new();
        for (i, lane) in lanes.iter_mut().enumerate() {
            if i == col {
                continue;
            }
            if *lane == Some(hash) {
                marks[i] = LaneMark::Merge;
                *lane = None;
                closing.insert(i);
            } else if lane.is_some() {
                marks[i] = LaneMark::Line;
            }
        }
        marks[col] = LaneMark::Node;
        // A merge edge crosses the live lanes between the node and the
        // closing column.
        for &i in &closing {
            mark_crossings(&mut marks, &lanes, col, i);
        }

        // Extra parents: continue an existing waiting lane (junction) or open
        // a new one (fork). A parent already placed above cannot be edged to.
        let mut parents = row.parents.iter().map(String::as_str);
        let first_parent = parents.next();
        for parent in parents {
            if seen.contains(parent) {
                continue;
            }
            if let Some(j) = lanes.iter().position(|l| *l == Some(parent)) {
                marks[j] = LaneMark::Junction;
                mark_crossings(&mut marks, &lanes, col, j);
                continue;
            }
            // Prefer a free column right of the node; do not reuse lanes that
            // closed into the node on this very row (their mark stays Merge).
            let free =
                (col + 1..lanes.len()).find(|&i| lanes[i].is_none() && !closing.contains(&i));
            match free {
                Some(t) if t < MAX_LANES => {
                    lanes[t] = Some(parent);
                    marks[t] = LaneMark::Fork;
                    mark_crossings(&mut marks, &lanes, col, t);
                }
                Some(_) => overflow = true,
                None if lanes.len() < MAX_LANES => {
                    lanes.push(Some(parent));
                    marks.push(LaneMark::Fork);
                    mark_crossings(&mut marks, &lanes, col, lanes.len() - 1);
                }
                None => overflow = true,
            }
        }

        // The first parent keeps the node's lane; a parent that was already
        // laid out above ends the lane instead of opening a zombie edge. A node
        // clipped past MAX_LANES is drawn but never tracked.
        if col < MAX_LANES {
            lanes[col] = match first_parent {
                Some(p) if !seen.contains(p) => Some(p),
                _ => None,
            };
        }

        seen.insert(hash);
        // Trailing free lanes only pad the next row; drop them.
        while lanes.last().is_some_and(|l| l.is_none()) {
            lanes.pop();
        }
        while marks.last() == Some(&LaneMark::Blank) {
            marks.pop();
        }
        cells.push(LaneCell { marks, overflow });
    }
    cells
}

/// Horizontal parts of an edge between `a` and `b` pass through the live lanes
/// strictly between them; mark those as [`LaneMark::Junction`].
fn mark_crossings(marks: &mut [LaneMark], lanes: &[Option<&str>], a: usize, b: usize) {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    for i in lo + 1..hi {
        if lanes[i].is_some() && marks[i] == LaneMark::Line {
            marks[i] = LaneMark::Junction;
        }
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod lanes_tests;
