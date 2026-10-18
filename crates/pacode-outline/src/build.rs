//! From the walk's kept rows to the public [`Outline`]: short gaps are shown after all
//! (a marker would not save anything), the remaining gaps become labelled hidden regions.

use std::ops::Range;

use crate::walk::Hint;
use crate::{Hidden, Outline, Symbol};

pub(crate) fn assemble(
    source: &str,
    mut keep: Vec<bool>,
    hints: &[Hint],
    symbols: Vec<Symbol>,
    min_hidden: usize,
) -> Outline {
    for gap in runs(&keep, false) {
        if gap.len() < min_hidden
            && let Some(rows) = keep.get_mut(gap)
        {
            rows.fill(true);
        }
    }
    let lines: Vec<&str> = source.lines().collect();
    let mut kept = Vec::new();
    let mut hidden = Vec::new();
    for (range, shown) in all_runs(&keep) {
        if shown {
            kept.push(range);
        } else {
            let label = label_for(&symbols, hints, &lines, range.clone());
            hidden.push(Hidden {
                lines: range,
                label,
            });
        }
    }
    Outline {
        total_lines: keep.len(),
        kept,
        hidden,
        symbols,
    }
}

/// Maximal runs of rows whose flag equals `value`.
fn runs(keep: &[bool], value: bool) -> Vec<Range<usize>> {
    all_runs(keep)
        .into_iter()
        .filter(|(_, v)| *v == value)
        .map(|(r, _)| r)
        .collect()
}

fn all_runs(keep: &[bool]) -> Vec<(Range<usize>, bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < keep.len() {
        let value = keep[start];
        let len = keep[start..].iter().take_while(|v| **v == value).count();
        out.push((start..start + len, value));
        start += len;
    }
    out
}

/// What a hidden region belongs to: the innermost named symbol that contains it, or
/// the import run when that is tighter. Blank rows at either end are ignored, so a
/// Python body followed by blank lines still names its function.
fn label_for(
    symbols: &[Symbol],
    hints: &[Hint],
    lines: &[&str],
    gap: Range<usize>,
) -> Option<String> {
    let blank = |row: usize| lines.get(row).is_none_or(|l| l.trim().is_empty());
    let (mut a, mut b) = (gap.start, gap.end);
    while b > a + 1 && blank(b - 1) {
        b -= 1;
    }
    while a + 1 < b && blank(a) {
        a += 1;
    }
    let mut best: Option<&Symbol> = None;
    innermost(symbols, a, b, &mut best);
    let best_span = best.map(|s| s.lines.len());
    let hint = hints
        .iter()
        .filter(|h| h.lines.start <= a && b <= h.lines.end)
        .filter(|h| best_span.is_none_or(|span| h.lines.len() < span))
        .min_by_key(|h| h.lines.len());
    if let Some(h) = hint {
        return Some(h.label.to_string());
    }
    best.map(|s| format!("{} {}", s.kind, s.name).trim().to_string())
}

fn innermost<'a>(symbols: &'a [Symbol], a: usize, b: usize, best: &mut Option<&'a Symbol>) {
    for symbol in symbols {
        if symbol.lines.start <= a && b <= symbol.lines.end {
            let tighter = best.is_none_or(|cur| symbol.lines.len() <= cur.lines.len());
            if !symbol.name.is_empty() && tighter {
                *best = Some(symbol);
            }
            innermost(&symbol.children, a, b, best);
        }
    }
}
