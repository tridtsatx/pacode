//! The pure applier: verify anchors, order and check ops, rebuild the content.

use std::ops::Range;

use super::RELOCATE_WINDOW;
use super::anchor::{Anchor, EditError, Op, StaleAnchor};
use super::hash::{decode, hash_code, line_hash, strip_anchor_prefix};
use super::lines::{Line, dominant_eol, split_lines};
use super::stale::{StaleHit, stale_message};

/// An anchor whose line number had drifted and was matched by hash instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// 1-based op number.
    pub op: usize,
    pub role: &'static str,
    /// Line number in the anchor.
    pub from: usize,
    /// Line number it was found at (original file).
    pub to: usize,
}

/// Result of a successful [`apply_ops`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub content: String,
    pub relocations: Vec<Relocation>,
    /// Lines inserted (including replacement lines).
    pub added: usize,
    /// Original lines removed (including replaced lines).
    pub removed: usize,
    /// Changed regions as 0-based line ranges of the NEW content, in file order;
    /// an empty range marks a pure deletion point.
    pub changed: Vec<Range<usize>>,
}

impl Applied {
    /// 1-based line of the first change in the new content.
    pub fn first_changed_line(&self) -> Option<usize> {
        self.changed.first().map(|r| r.start + 1)
    }
}

struct Span {
    start: usize,
    end: usize,
    lines: Vec<String>,
    op: usize,
}

impl Span {
    fn describe(&self) -> String {
        if self.start == self.end {
            format!("op {}: insertion at line {}", self.op, self.start + 1)
        } else {
            format!("op {}: lines {}-{}", self.op, self.start + 1, self.end)
        }
    }
}

enum Seg<'a> {
    Orig(usize),
    New(&'a str),
}

struct Resolver<'a, 'b> {
    lines: &'b [Line<'a>],
    stale: Vec<StaleHit>,
    relocations: Vec<Relocation>,
}

impl Resolver<'_, '_> {
    /// 0-based index of the line `anchor` designates, relocating within
    /// [`RELOCATE_WINDOW`] when the hash is unique there; records a stale hit otherwise.
    fn line(&mut self, op: usize, role: &'static str, anchor: &Anchor) -> Option<usize> {
        let Anchor::Line { line, hash } = anchor else {
            return None;
        };
        // A malformed hash (only possible for hand-built anchors) never matches: stale.
        let code = decode(hash).unwrap_or(u16::MAX);
        let n = self.lines.len();
        let idx = line.saturating_sub(1);
        if self
            .lines
            .get(idx)
            .is_some_and(|l| hash_code(l.text) == code)
        {
            return Some(idx);
        }
        let lo = idx.saturating_sub(RELOCATE_WINDOW);
        let hi = idx.saturating_add(RELOCATE_WINDOW + 1).min(n);
        let found: Vec<usize> = (lo..hi)
            .filter(|&i| i != idx)
            .filter(|&i| self.lines.get(i).is_some_and(|l| hash_code(l.text) == code))
            .collect();
        if let [only] = found.as_slice() {
            self.relocations.push(Relocation {
                op,
                role,
                from: *line,
                to: only + 1,
            });
            return Some(*only);
        }
        let now = match self.lines.get(idx) {
            Some(l) => format!("line {line} is now `{line}#{}`", line_hash(l.text)),
            None => format!("line {line} does not exist (file has {n} lines)"),
        };
        let why = if found.is_empty() {
            format!("hash not found within {RELOCATE_WINDOW} lines")
        } else {
            format!(
                "hash appears on {} lines within {RELOCATE_WINDOW} lines (ambiguous)",
                found.len()
            )
        };
        self.stale.push(StaleHit {
            info: StaleAnchor {
                op,
                role,
                anchor: anchor.to_string(),
                reason: format!("{now}; {why}"),
            },
            focus: idx.min(n.saturating_sub(1)),
        });
        None
    }
}

/// Split model-supplied lines on `\n`, drop `\r`, and strip pasted `LINE#HASH| ` prefixes
/// when every line carries one.
fn normalize_lines(raw: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        let s = s.strip_suffix('\n').unwrap_or(s);
        for part in s.split('\n') {
            out.push(part.strip_suffix('\r').unwrap_or(part).to_string());
        }
    }
    let cleaned: Option<Vec<String>> = out
        .iter()
        .map(|l| strip_anchor_prefix(l).map(str::to_string))
        .collect();
    match cleaned {
        Some(c) if !c.is_empty() => c,
        _ => out,
    }
}

fn require_line(op: usize, role: &'static str, a: &Anchor) -> Result<(), EditError> {
    match a {
        Anchor::Line { .. } => Ok(()),
        Anchor::Bof | Anchor::Eof => Err(EditError::NotALineAnchor { op, role }),
    }
}

fn validate(no: usize, op: &Op) -> Result<(), EditError> {
    match op {
        Op::Replace { start, end, .. } | Op::Delete { start, end } => {
            require_line(no, "start", start)?;
            if let Some(e) = end {
                require_line(no, "end", e)?;
            }
            Ok(())
        }
        Op::InsertAfter { lines, .. } | Op::InsertBefore { lines, .. } => {
            if lines.is_empty() {
                return Err(EditError::EmptyInsert { op: no });
            }
            Ok(())
        }
    }
}

/// Apply `ops` to `content`. All anchors refer to `content` as given; ops are validated
/// together and either all apply or none do. Inserts at the same gap keep op order.
/// Line terminators, mixed endings and a missing final newline are preserved.
pub fn apply_ops(content: &str, ops: &[Op]) -> Result<Applied, EditError> {
    if ops.is_empty() {
        return Err(EditError::NoOps);
    }
    for (i, op) in ops.iter().enumerate() {
        validate(i + 1, op)?;
    }
    let lines = split_lines(content);
    let n = lines.len();
    let mut rz = Resolver {
        lines: &lines,
        stale: Vec::new(),
        relocations: Vec::new(),
    };
    let mut spans: Vec<Span> = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let no = i + 1;
        match op {
            Op::Replace {
                start,
                end,
                lines: new,
            } => {
                if let Some((s, e)) = resolve_range(&mut rz, no, start, end.as_ref())? {
                    spans.push(Span {
                        start: s,
                        end: e + 1,
                        lines: normalize_lines(new),
                        op: no,
                    });
                }
            }
            Op::Delete { start, end } => {
                if let Some((s, e)) = resolve_range(&mut rz, no, start, end.as_ref())? {
                    spans.push(Span {
                        start: s,
                        end: e + 1,
                        lines: Vec::new(),
                        op: no,
                    });
                }
            }
            Op::InsertAfter { at, lines: new } => {
                let gap = match at {
                    Anchor::Bof => Some(0),
                    Anchor::Eof => Some(n),
                    Anchor::Line { .. } => rz.line(no, "at", at).map(|k| k + 1),
                };
                if let Some(p) = gap {
                    spans.push(Span {
                        start: p,
                        end: p,
                        lines: normalize_lines(new),
                        op: no,
                    });
                }
            }
            Op::InsertBefore { at, lines: new } => {
                let gap = match at {
                    Anchor::Bof => Some(0),
                    Anchor::Eof => Some(n),
                    Anchor::Line { .. } => rz.line(no, "at", at),
                };
                if let Some(p) = gap {
                    spans.push(Span {
                        start: p,
                        end: p,
                        lines: normalize_lines(new),
                        op: no,
                    });
                }
            }
        }
    }
    if !rz.stale.is_empty() {
        let message = stale_message(content, &rz.stale);
        let anchors = rz.stale.into_iter().map(|h| h.info).collect();
        return Err(EditError::Stale { anchors, message });
    }
    let relocations = rz.relocations;

    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&k| spans.get(k).map(|s| (s.start, s.end, s.op)));
    check_overlap(&spans, &order)?;
    Ok(assemble(&lines, &spans, &order, relocations))
}

fn resolve_range(
    rz: &mut Resolver<'_, '_>,
    no: usize,
    start: &Anchor,
    end: Option<&Anchor>,
) -> Result<Option<(usize, usize)>, EditError> {
    let s = rz.line(no, "start", start);
    let e = match end {
        Some(a) => rz.line(no, "end", a),
        None => s,
    };
    match (s, e) {
        (Some(s), Some(e)) if e < s => Err(EditError::ReversedRange {
            op: no,
            start: s + 1,
            end: e + 1,
        }),
        (Some(s), Some(e)) => Ok(Some((s, e))),
        _ => Ok(None),
    }
}

fn check_overlap(spans: &[Span], order: &[usize]) -> Result<(), EditError> {
    let mut widest: Option<&Span> = None;
    for sp in order.iter().filter_map(|&k| spans.get(k)) {
        if let Some(prev) = widest {
            if sp.start < prev.end {
                return Err(EditError::Overlap {
                    first: prev.op.min(sp.op),
                    second: prev.op.max(sp.op),
                    detail: format!("{} vs {}", prev.describe(), sp.describe()),
                });
            }
            if sp.end >= prev.end {
                widest = Some(sp);
            }
        } else {
            widest = Some(sp);
        }
    }
    Ok(())
}

fn assemble(
    lines: &[Line<'_>],
    spans: &[Span],
    order: &[usize],
    relocations: Vec<Relocation>,
) -> Applied {
    let n = lines.len();
    let mut segs: Vec<Seg<'_>> = Vec::with_capacity(n);
    let mut changed = Vec::with_capacity(spans.len());
    let (mut added, mut removed, mut cursor) = (0, 0, 0);
    for sp in order.iter().filter_map(|&k| spans.get(k)) {
        segs.extend((cursor..sp.start).map(Seg::Orig));
        let first = segs.len();
        segs.extend(sp.lines.iter().map(|l| Seg::New(l.as_str())));
        changed.push(first..segs.len());
        added += sp.lines.len();
        removed += sp.end - sp.start;
        cursor = cursor.max(sp.end);
    }
    segs.extend((cursor..n).map(Seg::Orig));

    let default_eol = dominant_eol(lines);
    let no_final_newline = lines.last().is_some_and(|l| l.eol.is_empty());
    let mut content = String::new();
    let total = segs.len();
    for (k, seg) in segs.iter().enumerate() {
        let (text, own_eol) = match seg {
            Seg::Orig(i) => lines.get(*i).map_or(("", ""), |l| (l.text, l.eol)),
            Seg::New(t) => (*t, ""),
        };
        content.push_str(text);
        if k + 1 == total && no_final_newline {
            continue;
        }
        content.push_str(if own_eol.is_empty() {
            default_eol
        } else {
            own_eol
        });
    }
    Applied {
        content,
        relocations,
        added,
        removed,
        changed,
    }
}
