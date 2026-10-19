//! File versions reduced to hashes, and the line diff between two versions.

use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Content above this many bytes gets a whole-content hash only (G5): no per-line
/// hashes (8 bytes per line, plus the diff cost) are kept for it. Same limit as the
/// `read` tool's whole-file limit.
pub const MAX_LINE_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// One version of a file: the whole-content hash (authoritative identity) plus one
/// hash per line (to locate what changed between versions). Shared by `Arc` between
/// the path's current version and every agent that saw it. Above
/// [`MAX_LINE_SNAPSHOT_BYTES`] `lines` stays empty and `whole_only` is set: a diff
/// against such a version reports one full-file change.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub hash: u64,
    pub lines: Vec<u64>,
    pub whole_only: bool,
    /// Number of lines (kept even when `lines` is empty).
    pub total_lines: usize,
}

impl Snapshot {
    pub fn of(content: &str) -> Arc<Self> {
        let hash = hash_bytes(content.as_bytes());
        if content.len() > MAX_LINE_SNAPSHOT_BYTES {
            let newlines = content.bytes().filter(|&b| b == b'\n').count();
            let tail = usize::from(!content.ends_with('\n'));
            return Arc::new(Self {
                hash,
                lines: Vec::new(),
                whole_only: true,
                total_lines: newlines + tail,
            });
        }
        let lines = line_hashes(content);
        Arc::new(Self {
            hash,
            total_lines: lines.len(),
            lines,
            whole_only: false,
        })
    }

    /// Placeholder for a version whose content was never hashed (a path first seen
    /// through a ranged read of a large file). Diffs against it are full-file changes.
    pub fn unknown() -> Arc<Self> {
        Arc::new(Self {
            hash: 0,
            lines: Vec::new(),
            whole_only: true,
            total_lines: 0,
        })
    }

    /// Lines in the version.
    pub fn line_count(&self) -> usize {
        self.total_lines
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
        .map(|seg| hash_line(seg.strip_suffix('\n').unwrap_or(seg)))
        .collect()
}

/// Hash of one line's text (without its `\n`); a trailing `\r` is dropped, so it
/// equals the entry [`line_hashes`] produces for that line.
pub fn hash_line(text: &str) -> u64 {
    hash_bytes(text.strip_suffix('\r').unwrap_or(text).as_bytes())
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

/// Changed regions between two versions. A side without line hashes makes the whole
/// file one change (`old` = all of `old`'s lines, `new` = all of `new`'s).
pub fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> Vec<Change> {
    if old.hash == new.hash {
        return Vec::new();
    }
    if old.whole_only || new.whole_only {
        return vec![Change {
            old: 0..old.total_lines,
            new: 0..new.total_lines,
        }];
    }
    diff(&old.lines, &new.lines)
}

/// Lines of `current` (0-based ranges; points for pure insertions) that writing
/// `proposed` over it would change. Large texts skip the line diff: the region
/// between their common line prefix and suffix is reported as one range (a superset
/// of the exact answer, so only more conservative).
pub fn touched(current: &str, proposed: &str) -> Vec<Range<usize>> {
    if current.len().max(proposed.len()) > MAX_LINE_SNAPSHOT_BYTES {
        return touched_coarse(current, proposed);
    }
    diff(&line_hashes(current), &line_hashes(proposed))
        .into_iter()
        .map(|c| c.old)
        .collect()
}

/// Single-range superset of [`touched`]: lines of `current` between the common
/// whole-line prefix and suffix of the two texts.
pub fn touched_coarse(current: &str, proposed: &str) -> Vec<Range<usize>> {
    if current == proposed {
        return Vec::new();
    }
    let (c, p) = (current.as_bytes(), proposed.as_bytes());
    let common = c.iter().zip(p).take_while(|(a, b)| a == b).count();
    // Whole lines only: cut the shared prefix after its last newline.
    let prefix = c[..common]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let room = c.len().min(p.len()) - prefix;
    let shared = c
        .iter()
        .rev()
        .zip(p.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    // The suffix must start on a line start: drop everything up to its first newline.
    let tail = &c[c.len() - shared..];
    let suffix = tail
        .iter()
        .position(|&b| b == b'\n')
        .map_or(0, |i| shared - (i + 1));
    let start = c[..prefix].iter().filter(|&&b| b == b'\n').count();
    let end = c[..c.len() - suffix]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    // A tail without a final newline counts as a line of its own.
    let end = if suffix == 0 && !current.ends_with('\n') && !current.is_empty() {
        end + 1
    } else {
        end
    };
    #[allow(clippy::single_range_in_vec_init)]
    let ranges = vec![start..end.max(start)];
    ranges
}
