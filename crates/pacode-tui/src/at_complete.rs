//! Ignore-aware path completion for `@` references (spec prompt-input feature 2).
//!
//! Candidates come from the session cwd, respecting `.gitignore` and skipping `.git`.
//! Depth is strictly bounded to 1 per directory level to never walk the full tree on a keystroke.
//! Results are capped to a handful of rows (`MAX_AT_CANDIDATES`).

use std::path::Path;

#[cfg(test)]
#[path = "at_complete_tests.rs"]
mod at_complete_tests;

/// Maximum candidates displayed in the `@` reference popup.
pub const MAX_AT_CANDIDATES: usize = 20;

/// A path candidate for `@` completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtCandidate {
    /// Relative path with directory suffix `/` if it is a directory.
    pub path: String,
    pub is_dir: bool,
}

/// Gather candidates matching `query` relative to `cwd`.
///
/// Query is the partially typed path after `@`, e.g. `""`, `"src"`, `"src/"`, or `"src/pa"`.
pub fn complete_at_path(query: &str, cwd: &Path) -> Vec<AtCandidate> {
    let mut candidates = Vec::new();

    // Canonicalize cwd to resolve symlinks
    let canonical_cwd = match cwd.canonicalize() {
        Ok(c) => c,
        Err(_) => return candidates,
    };

    let (dir_prefix, name_prefix) = match query.rfind('/') {
        Some(idx) => (&query[..=idx], &query[idx + 1..]),
        None => ("", query),
    };

    let target_dir = if dir_prefix.is_empty() {
        canonical_cwd.clone()
    } else {
        canonical_cwd.join(dir_prefix)
    };

    if !target_dir.is_dir() {
        return candidates;
    }

    // Single directory level: never walk whole tree
    let mut builder = ignore::WalkBuilder::new(&target_dir);
    builder
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .parents(true);

    // If typing hidden file, allow hidden files
    if name_prefix.starts_with('.') {
        builder.hidden(false);
    }

    let mut entries: Vec<(String, bool)> = Vec::new();

    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip the target directory itself
        if entry.depth() == 0 {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name == ".git" {
            continue;
        }

        if file_name.starts_with(name_prefix) {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            entries.push((file_name, is_dir));
        }
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });

    for (name, is_dir) in entries {
        let suffix = if is_dir { "/" } else { "" };
        let full_path = format!("{dir_prefix}{name}{suffix}");
        candidates.push(AtCandidate {
            path: full_path,
            is_dir,
        });

        if candidates.len() >= MAX_AT_CANDIDATES {
            break;
        }
    }

    candidates
}
