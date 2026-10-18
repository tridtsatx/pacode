//! Pure text helpers: normalisation, tokenisation, Jaccard similarity, FTS5 query building.
//! No caches, no state.

use std::collections::BTreeSet;

/// Collapse every whitespace run (including newlines) into one space.
pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keep at most `max` characters (never splits a code point).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Lowercased alphanumeric tokens in order (Unicode aware, so Cyrillic works).
pub fn token_list(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Title key used for exact duplicate detection.
pub fn normalize_title(s: &str) -> String {
    token_list(s).join(" ")
}

/// Token set (tokens of 2+ chars) used for Jaccard similarity.
pub fn token_set(s: &str) -> BTreeSet<String> {
    token_list(s)
        .into_iter()
        .filter(|t| t.chars().count() >= 2)
        .collect()
}

/// Jaccard similarity of two sets; two empty sets are not similar (0.0).
pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Build an FTS5 MATCH expression: tokens OR-ed, tokens of 3+ chars as prefixes.
/// Tokens are purely alphanumeric so quoting needs no escaping. `None` when the
/// text has no tokens.
pub fn fts_query(text: &str) -> Option<String> {
    let mut seen = BTreeSet::new();
    let mut toks: Vec<String> = Vec::new();
    for t in token_list(text) {
        if t.chars().count() >= 2 && seen.insert(t.clone()) {
            toks.push(t);
        }
        if toks.len() >= 16 {
            break;
        }
    }
    if toks.is_empty() {
        toks = token_list(text).into_iter().take(16).collect();
    }
    if toks.is_empty() {
        return None;
    }
    let parts: Vec<String> = toks
        .iter()
        .map(|t| {
            if t.chars().count() >= 3 {
                format!("\"{t}\"*")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect();
    Some(parts.join(" OR "))
}
