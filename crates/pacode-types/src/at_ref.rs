//! Shared logic for `@` file references (spec §11).
//!
//! Rule:
//! A `@` starts a file reference IF AND ONLY IF it is at index 0 or preceded by whitespace.
//! An `@` mid-word (such as in an email `user@example.com` or identifier `foo@bar`) is NOT a reference.

use std::ops::Range;

#[cfg(test)]
#[path = "at_ref_tests.rs"]
mod at_ref_tests;

/// Represents an `@path` reference found in prompt text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtRefSpan {
    /// Byte range of the full `@path` token in the source text.
    pub range: Range<usize>,
    /// The relative path portion without the leading `@`.
    pub path: String,
}

/// Active query when the cursor is currently on or inside an `@path` reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtQuery {
    /// Byte index of the starting `@`.
    pub at_byte_index: usize,
    /// Partial path typed so far between `@` and cursor.
    pub query: String,
}

/// Returns true if character preceding byte index `at_idx` allows `@` to begin a reference.
#[inline]
pub fn is_reference_start(text: &str, at_idx: usize) -> bool {
    if at_idx == 0 {
        true
    } else {
        text[..at_idx]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace())
    }
}

/// Parses all valid `@path` references from text.
pub fn parse_references(text: &str) -> Vec<AtRefSpan> {
    let mut refs = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'@' && is_reference_start(text, i) {
            let start = i;
            let path_start = i + 1;
            let mut end = path_start;
            while end < bytes.len() {
                if let Some(ch) = text[end..].chars().next() {
                    if ch.is_whitespace() {
                        break;
                    }
                    end += ch.len_utf8();
                } else {
                    break;
                }
            }
            let raw_path = &text[path_start..end];
            // Strip trailing sentence punctuation: . , ; : ? ! ) ] }
            let trimmed_path =
                raw_path.trim_end_matches(['.', ',', ';', ':', '?', '!', ')', ']', '}']);
            if !trimmed_path.is_empty() {
                let trimmed_end = path_start + trimmed_path.len();
                refs.push(AtRefSpan {
                    range: start..trimmed_end,
                    path: trimmed_path.to_string(),
                });
            }
            i = end;
        } else if let Some(ch) = text[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    refs
}

/// Checks if `cursor_bytes` is within or immediately after an active `@` reference.
pub fn find_active_query(text: &str, cursor_bytes: usize) -> Option<AtQuery> {
    let cursor = cursor_bytes.min(text.len());
    let prefix = &text[..cursor];

    // Find the last '@' in prefix
    let at_idx = prefix.rfind('@')?;
    if !is_reference_start(text, at_idx) {
        return None;
    }

    // Check if there is any whitespace between '@' and cursor
    let query_part = &prefix[at_idx + 1..];
    if query_part.chars().any(|c| c.is_whitespace()) {
        return None;
    }

    Some(AtQuery {
        at_byte_index: at_idx,
        query: query_part.to_string(),
    })
}
