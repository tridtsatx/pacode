//! Evaluate rules over the ADDED lines of a file.

use std::path::Path;

use regex::Regex;

use crate::diff::{OldLines, added_by_set, added_line_indices};
use crate::mask::{Syntax, mask_code, rust_test_lines};
use crate::rule::{Scope, Severity};
use crate::ruleset::{ModeFilter, RuleSet, ext_of};

/// Hits reported per rule per scan; the rest are counted in `suppressed`.
pub const MAX_FINDINGS_PER_RULE: usize = 5;
/// Hits reported per scan over all rules.
pub const MAX_FINDINGS_TOTAL: usize = 20;
/// Lines longer than this (minified code) are not scanned.
const MAX_LINE_BYTES: usize = 4000;
const SNIPPET_CHARS: usize = 120;
/// How far above a hit `unless_above` looks.
const ABOVE_LOOKBACK: usize = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    /// 1-based line in the new text.
    pub line: usize,
    pub snippet: String,
    pub message: String,
    pub max_per_turn: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub path: String,
    pub findings: Vec<Finding>,
    /// Hits dropped by the per-rule / total caps.
    pub suppressed: usize,
}

impl ScanReport {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn has_block(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Block)
    }

    /// Distinct rule names in first-hit order.
    pub fn rule_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for f in &self.findings {
            if !names.contains(&f.rule) {
                names.push(f.rule.clone());
            }
        }
        names
    }
}

/// Scan the lines of `new` that differ from `old` (exact diff). `old = None` = new file.
pub fn scan_added(
    set: &RuleSet,
    path: &Path,
    old: Option<&str>,
    new: &str,
    filter: ModeFilter,
) -> ScanReport {
    let added = added_line_indices(old, new);
    scan_lines(set, path, new, &added, filter)
}

/// Like [`scan_added`] but with the cheap set-based approximation, for repeated
/// scans of a growing text (streaming).
pub fn scan_added_fast(
    set: &RuleSet,
    path: &Path,
    old: &OldLines,
    new: &str,
    filter: ModeFilter,
) -> ScanReport {
    let added = added_by_set(old, new);
    scan_lines(set, path, new, &added, filter)
}

/// Scan the given 0-based line indexes of `new`.
pub fn scan_lines(
    set: &RuleSet,
    path: &Path,
    new: &str,
    added: &[usize],
    filter: ModeFilter,
) -> ScanReport {
    let mut report = ScanReport {
        path: path.display().to_string(),
        ..ScanReport::default()
    };
    if added.is_empty() {
        return report;
    }
    let rules = set.applicable(path, filter);
    if rules.is_empty() {
        return report;
    }
    let ext = ext_of(path);
    let raw: Vec<&str> = new.lines().collect();
    let needs_mask = rules
        .iter()
        .any(|r| r.scope == Scope::Code || r.skip_test_code);
    let masked_text = if needs_mask {
        Syntax::for_ext(&ext).map(|syntax| mask_code(new, syntax))
    } else {
        None
    };
    let masked: Vec<&str> = masked_text
        .as_deref()
        .map(|m| m.lines().collect())
        .unwrap_or_default();
    let masked_ok = masked.len() == raw.len();
    let test_flags = match masked_text.as_deref() {
        Some(m) if masked_ok && ext == "rs" && rules.iter().any(|r| r.skip_test_code) => {
            rust_test_lines(m)
        }
        _ => Vec::new(),
    };

    let mut per_rule = vec![0usize; rules.len()];
    for &idx in added {
        let Some(&raw_line) = raw.get(idx) else {
            continue;
        };
        if raw_line.len() > MAX_LINE_BYTES {
            continue;
        }
        for (ri, rule) in rules.iter().enumerate() {
            if rule.skip_test_code && test_flags.get(idx) == Some(&true) {
                continue;
            }
            let text = if rule.scope == Scope::Code && masked_ok {
                masked[idx]
            } else {
                raw_line
            };
            let suppressed_by_unless = |start: usize| {
                rule.unless.as_ref().is_some_and(|unless| {
                    raw_line
                        .get(start..)
                        .is_some_and(|rest| unless.is_match(rest))
                })
            };
            if rule
                .pattern
                .find_iter(text)
                .all(|hit| suppressed_by_unless(hit.start()))
            {
                continue;
            }
            if let Some(above) = &rule.unless_above
                && above_matches(&raw, idx, above)
            {
                continue;
            }
            if per_rule[ri] >= MAX_FINDINGS_PER_RULE || report.findings.len() >= MAX_FINDINGS_TOTAL
            {
                report.suppressed += 1;
                continue;
            }
            per_rule[ri] += 1;
            report.findings.push(Finding {
                rule: rule.name.clone(),
                severity: rule.severity,
                line: idx + 1,
                snippet: snippet(raw_line),
                message: rule.message.clone(),
                max_per_turn: rule.max_per_turn,
            });
        }
    }
    report
}

/// Does `re` match a comment/attribute line in the contiguous block above `idx`?
fn above_matches(raw: &[&str], idx: usize, re: &Regex) -> bool {
    for j in (idx.saturating_sub(ABOVE_LOOKBACK)..idx).rev() {
        let t = raw[j].trim();
        let is_comment_or_attr = t.starts_with("//")
            || t.starts_with("/*")
            || t.starts_with('*')
            || t.starts_with("#[")
            || t.starts_with("#!");
        if !is_comment_or_attr {
            return false;
        }
        if re.is_match(t) {
            return true;
        }
    }
    false
}

fn snippet(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() <= SNIPPET_CHARS {
        return t.to_string();
    }
    let cut: String = t.chars().take(SNIPPET_CHARS).collect();
    format!("{cut}…")
}
