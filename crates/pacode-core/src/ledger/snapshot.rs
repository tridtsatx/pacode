//! File versions reduced to hashes, and the line diff between two versions.

use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// One version of a file: the whole-content hash (authoritative identity) plus one
/// hash per line (to locate what changed between versions). Shared by `Arc` between
/// the path's current version and every agent that saw it.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub hash: u64,
    pub lines: Vec<u64>,
}

impl Snapshot {
    pub fn of(content: &str) -> Arc<Self> {
        Arc::new(Self {
            hash: hash_bytes(content.as_bytes()),
            lines: line_hashes(content),
        })
    }

    /// Short display form of the version, e.g. `3f2a91c0`.
    pub fn short(&self) -> String {
        format!("{:08x}", self.hash >> 32)
    }
}

/// Cheap metadata compared before rehashing (bash side effects): never authoritative,
/// a mismatch only means "read and hash again".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

/// SipHash with fixed keys: stable within the process, which is all an in-memory
/// ledger needs.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

/// One hash per line, split like the hashline module (`\n`, a trailing `\r` dropped).
pub fn line_hashes(content: &str) -> Vec<u64> {
    content
        .split_inclusive('\n')
        .map(|seg| {
            let text = seg.strip_suffix('\n').unwrap_or(seg);
            let text = text.strip_suffix('\r').unwrap_or(text);
            hash_bytes(text.as_bytes())
        })
        .collect()
}

/// A region that differs between two versions: `old` lines (0-based, half-open) were
/// replaced by `new` lines. An empty range is an insertion or deletion point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

impl Change {
    /// Lines affected, counting the larger side.
    pub fn size(&self) -> usize {
        (self.old.end - self.old.start).max(self.new.end - self.new.start)
    }
}

/// Myers gives up after this and reports the rest as one change: a coarser diff is
/// only more conservative (more lines count as changed).
const DIFF_DEADLINE: Duration = Duration::from_millis(20);

/// Changed regions between two line-hash sequences, in file order.
pub fn diff(old: &[u64], new: &[u64]) -> Vec<Change> {
    let deadline = Instant::now().checked_add(DIFF_DEADLINE);
    let ops = similar::capture_diff_slices_deadline(similar::Algorithm::Myers, old, new, deadline);
    let mut out: Vec<Change> = Vec::new();
    for op in ops {
        if let similar::DiffOp::Equal { .. } = op {
            continue;
        }
        let (o, n) = (op.old_range(), op.new_range());
        match out.last_mut() {
            Some(prev) if prev.old.end == o.start && prev.new.end == n.start => {
                prev.old.end = o.end;
                prev.new.end = n.end;
            }
            _ => out.push(Change { old: o, new: n }),
        }
    }
    out
}

/// Lines of `current` (0-based ranges; points for pure insertions) that writing
/// `proposed` over it would change.
pub fn touched(current: &str, proposed: &str) -> Vec<Range<usize>> {
    diff(&line_hashes(current), &line_hashes(proposed))
        .into_iter()
        .map(|c| c.old)
        .collect()
}
