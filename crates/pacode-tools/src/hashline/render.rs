//! Display of anchored lines: the `read` format and context snippets.

use std::ops::Range;

use super::MAX_DISPLAY_CHARS;
use super::hash::line_hash;
use super::lines::split_lines;

/// One display row `LINE#HASH| text`; very long lines are cut (hash covers the full line).
pub fn annotated_line(number: usize, text: &str) -> String {
    let hash = line_hash(text);
    match text.char_indices().nth(MAX_DISPLAY_CHARS) {
        Some((cut, _)) => {
            let omitted = text.chars().count() - MAX_DISPLAY_CHARS;
            format!(
                "{number}#{hash}| {}...[{omitted} more chars]",
                text.get(..cut).unwrap_or(text)
            )
        }
        None => format!("{number}#{hash}| {text}"),
    }
}

/// Annotate consecutive lines, numbering from `start_line` (1-based), joined by `\n`.
pub fn annotate_lines<'a>(lines: impl IntoIterator<Item = &'a str>, start_line: usize) -> String {
    let mut out = String::new();
    for (k, text) in lines.into_iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        out.push_str(&annotated_line(start_line + k, text));
    }
    out
}

/// Annotate every line of `content`; the first line is numbered `start_line`.
pub fn annotate(content: &str, start_line: usize) -> String {
    let lines = split_lines(content);
    annotate_lines(lines.iter().map(|l| l.text), start_line)
}

/// Anchored lines around the 0-based line ranges in `focus` (an empty range marks a
/// gap), `context` lines each side, windows merged and separated by `...`, at most
/// `max_lines` rows. With `mark`, focus lines are prefixed `> ` and others two spaces.
pub fn snippet(
    content: &str,
    focus: &[Range<usize>],
    context: usize,
    max_lines: usize,
    mark: bool,
) -> String {
    let lines = split_lines(content);
    let n = lines.len();
    if n == 0 {
        return "(file is empty)".to_string();
    }
    let last = n - 1;
    let mut windows: Vec<(usize, usize)> = focus
        .iter()
        .map(|r| {
            let start = r.start.min(last);
            let end = r.end.max(start + 1).min(n);
            (start.saturating_sub(context), (end + context).min(n))
        })
        .collect();
    windows.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in windows {
        if let Some(prev) = merged.last_mut()
            && a <= prev.1
        {
            prev.1 = prev.1.max(b);
            continue;
        }
        merged.push((a, b));
    }
    let in_focus = |i: usize| {
        focus
            .iter()
            .any(|r| r.contains(&i) || (r.is_empty() && r.start.min(last) == i))
    };
    let mut rows: Vec<String> = Vec::new();
    let mut hidden = 0usize;
    for (w, (a, b)) in merged.iter().enumerate() {
        if w > 0 && rows.len() < max_lines {
            rows.push("...".to_string());
        }
        for (i, line) in lines.iter().enumerate().take(*b).skip(*a) {
            if rows.len() >= max_lines {
                hidden += 1;
                continue;
            }
            let row = annotated_line(i + 1, line.text);
            rows.push(if mark {
                format!("{} {row}", if in_focus(i) { ">" } else { " " })
            } else {
                row
            });
        }
    }
    if hidden > 0 {
        rows.push(format!("... ({hidden} more lines not shown)"));
    }
    rows.join("\n")
}
