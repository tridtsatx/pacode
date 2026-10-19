//! Ranged reads of files too large to hash line by line (G5 / H2 trade-off).
//!
//! `read` streams a line range of a file above 8 MiB and never holds the whole content,
//! so the ledger cannot snapshot it. [`Partial`] records what the agent was shown
//! instead: the hashes of the seen lines plus the file's len/mtime. The whole-file
//! hash is deferred to the agent's next write check, which has the full text:
//! - the file's len/mtime still equal the read's: the agent's view is the current
//!   version ([`Resolution::Unchanged`]);
//! - the file changed, but every line the write touches still hashes like the line the
//!   agent saw ([`Resolution::Covers`]): the write only replaces text the agent knew;
//! - otherwise the agent must re-read ([`Resolution::Stale`], a whole-file conflict).
//!
//! Cap: [`MAX_SEGMENTS`] ranges and [`MAX_PARTIAL_LINES`] hashed lines per (agent,
//! path), oldest dropped first (forgetting only costs a re-read). Invalidation: a
//! newer read at the same len/mtime merges, a different stat replaces; a hashed
//! `record_read`/`record_write` drops it. Owner: `Seen` in the path's ledger entry.

use std::ops::Range;

use super::ranges::LineRanges;
use super::snapshot::{Stat, line_hashes};

/// Read ranges remembered per (agent, path).
pub const MAX_SEGMENTS: usize = 8;
/// Line hashes remembered per (agent, path) across its segments.
pub const MAX_PARTIAL_LINES: usize = 20_000;

/// Same len and a known, equal mtime. A missing mtime never matches: len alone is no
/// proof the content is unchanged.
pub fn same_stat(a: &Stat, b: &Stat) -> bool {
    a.len == b.len && a.mtime.is_some() && a.mtime == b.mtime
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    /// 0-based first line.
    start: usize,
    hashes: Vec<u64>,
}

/// How a pending ranged read relates to the file as it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Unchanged,
    Covers,
    Stale,
}

/// What an agent saw of a large file, at the file stat it saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partial {
    stat: Stat,
    segments: Vec<Segment>,
}

impl Partial {
    /// `prev` extended with the lines `start..` (hashes `hashes`) read at `stat`.
    /// A different stat starts over: the older ranges describe another version.
    pub fn merged(prev: Option<&Partial>, stat: Stat, start: usize, hashes: Vec<u64>) -> Self {
        let mut segments = match prev {
            Some(p) if same_stat(&p.stat, &stat) => p.segments.clone(),
            _ => Vec::new(),
        };
        segments.push(Segment { start, hashes });
        while segments.len() > MAX_SEGMENTS
            || (segments.len() > 1
                && segments.iter().map(|s| s.hashes.len()).sum::<usize>() > MAX_PARTIAL_LINES)
        {
            segments.remove(0);
        }
        Self { stat, segments }
    }

    /// The lines covered, as ranges.
    pub fn ranges(&self) -> LineRanges {
        let mut out = LineRanges::default();
        for s in &self.segments {
            out.insert(s.start..s.start + s.hashes.len());
        }
        out
    }

    /// Compare with the file as it is now: `now` its stat, `text` its content,
    /// `lines` its line count, `touched` the lines the pending write changes (`None` =
    /// the edit could not be applied).
    pub fn resolve(
        &self,
        now: &Stat,
        text: &str,
        lines: usize,
        touched: Option<&[Range<usize>]>,
    ) -> Resolution {
        if same_stat(&self.stat, now) {
            return Resolution::Unchanged;
        }
        let Some(touched) = touched else {
            return Resolution::Stale;
        };
        let verified = self.verified(text);
        let covered = touched.iter().all(|r| {
            let span = if r.is_empty() {
                // An insertion point: both neighbours must be lines the agent knows.
                r.start.saturating_sub(1)..(r.start + 1).min(lines)
            } else {
                r.clone()
            };
            verified.covers(&span)
        });
        if covered {
            Resolution::Covers
        } else {
            Resolution::Stale
        }
    }

    /// Lines of `text` that still hash like what the agent saw there.
    fn verified(&self, text: &str) -> LineRanges {
        let mut out = LineRanges::default();
        let last = self
            .segments
            .iter()
            .map(|s| s.start + s.hashes.len())
            .max()
            .unwrap_or(0);
        // One pass over the lines up to the furthest seen one.
        let hashes: Vec<u64> = line_hashes_upto(text, last);
        for s in &self.segments {
            let mut run: Option<usize> = None;
            for (i, seen) in s.hashes.iter().enumerate() {
                let same = hashes.get(s.start + i) == Some(seen);
                match (same, run) {
                    (true, None) => run = Some(s.start + i),
                    (false, Some(from)) => {
                        out.insert(from..s.start + i);
                        run = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = run {
                out.insert(from..s.start + s.hashes.len());
            }
        }
        out
    }
}

/// Hashes of the first `n` lines of `text` (same splitting as `line_hashes`).
fn line_hashes_upto(text: &str, n: usize) -> Vec<u64> {
    let end = text
        .match_indices('\n')
        .nth(n.saturating_sub(1))
        .map_or(text.len(), |(i, _)| i + 1);
    if n == 0 {
        return Vec::new();
    }
    line_hashes(&text[..end])
}
