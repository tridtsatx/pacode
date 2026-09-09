//! Expansion of `@` file references for user messages (prompt-input feature 2).
//!
//! When a `Request::UserMessage` arrives, `@path` tokens are resolved relative to
//! the session `cwd` and their contents attached to what the model sees.
//! References are bounded per spec §6.4 (`tool_output_cap_chars`, 16K chars default).
//! Non-existent, outside-cwd, or unreadable references remain plain text and never fail the turn.

use std::path::Path;

#[cfg(test)]
#[path = "expand_tests.rs"]
mod expand_tests;

/// Checks if a file path is a supported image extension.
pub fn is_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
}

/// Expands `@path` references in `text` against `cwd`, returning the message text
/// that the model will receive.
pub fn expand_user_message(text: &str, cwd: &Path, cap_chars: usize) -> String {
    let refs = pacode_types::at_ref::parse_references(text);
    if refs.is_empty() {
        return text.to_string();
    }

    let canonical_cwd = match cwd.canonicalize() {
        Ok(c) => c,
        Err(_) => return text.to_string(),
    };

    let mut attachments = Vec::new();

    for r in &refs {
        let target_path = cwd.join(&r.path);
        if !target_path.exists() || !target_path.is_file() {
            continue;
        }

        let canonical_target = match target_path.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Must be within session cwd (prevent directory traversal).
        if !canonical_target.starts_with(&canonical_cwd) {
            continue;
        }

        if is_image_path(&r.path) {
            // Per spec §18.5, model vision is not supported in v1; image preview metadata is attached.
            if let Ok(meta) = std::fs::metadata(&target_path) {
                attachments.push(format!(
                    "<attachment path=\"{}\">\n[image file: {} ({} bytes)]\n</attachment>",
                    r.path,
                    r.path,
                    meta.len()
                ));
            }
            continue;
        }

        let bytes = match std::fs::read(&target_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Binary check: null byte in first 8 KiB.
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            continue;
        }

        let content_str = String::from_utf8_lossy(&bytes);
        let bounded = pacode_types::truncate_head_tail(&content_str, cap_chars);
        attachments.push(format!(
            "<attachment path=\"{}\">\n{bounded}\n</attachment>",
            r.path
        ));
    }

    if attachments.is_empty() {
        text.to_string()
    } else {
        let mut out = text.to_string();
        for att in attachments {
            out.push_str("\n\n");
            out.push_str(&att);
        }
        out
    }
}
