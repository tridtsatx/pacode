//! Bash mode completion (spec prompt-input feature 1).
//!
//! Two sources only:
//! 1. Bounded bash history (`BASH_HISTORY_CAP` = 500 entries, FIFO eviction via `pop_front()`,
//!    owned by `InputState` and freed when `AppState` drops at client termination).
//! 2. Filesystem paths relative to the session cwd, with directories suffixed by `/`.
//!    Does NOT scan `$PATH`.

use std::collections::VecDeque;
use std::path::Path;

#[cfg(test)]
#[path = "complete_tests.rs"]
mod complete_tests;

/// Maximum entries kept in bash history buffer.
/// FIFO eviction: when history reaches 500, oldest entries are dropped.
/// Freed when `AppState` is dropped on client exit.
pub const BASH_HISTORY_CAP: usize = 500;

/// Maximum completion candidates returned to avoid unbounded memory/UI layout.
pub const MAX_COMPLETIONS: usize = 50;

/// Record a command in the bounded bash history.
pub fn record_history(history: &mut VecDeque<String>, cmd: &str) {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return;
    }
    // Avoid duplicate if same as most recent
    if history.back().map(|s| s.as_str()) == Some(trimmed) {
        return;
    }
    history.push_back(trimmed.to_string());
    while history.len() > BASH_HISTORY_CAP {
        history.pop_front();
    }
}

/// A completion candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub display: String,
    pub replacement: String,
}

/// Complete from history and filesystem.
pub fn complete(
    input_after_bang: &str,
    cursor: usize,
    history: &VecDeque<String>,
    cwd: &Path,
) -> Vec<Candidate> {
    let mut results = Vec::new();
    let trimmed = input_after_bang.trim_start();
    let prefix = if cursor <= input_after_bang.len() {
        &input_after_bang[..cursor]
    } else {
        input_after_bang
    };

    // Source 1: Previously executed bash commands matching prefix
    if !trimmed.is_empty() {
        for hist_cmd in history.iter().rev() {
            if hist_cmd.starts_with(trimmed) && hist_cmd != trimmed {
                let cand = Candidate {
                    display: hist_cmd.clone(),
                    replacement: hist_cmd.clone(),
                };
                if !results.contains(&cand) {
                    results.push(cand);
                    if results.len() >= MAX_COMPLETIONS {
                        return results;
                    }
                }
            }
        }
    }

    // Source 2: Filesystem paths for the token under the cursor
    // Extract token before cursor
    let token = prefix.split_whitespace().last().unwrap_or("");

    let fs_candidates = complete_filesystem(token, cwd);
    for fc in fs_candidates {
        if results.len() >= MAX_COMPLETIONS {
            break;
        }
        if !results.contains(&fc) {
            results.push(fc);
        }
    }

    results
}

/// Find filesystem completion candidates relative to `cwd`.
pub fn complete_filesystem(token: &str, cwd: &Path) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    let (dir_part, file_prefix) = match token.rfind('/') {
        Some(idx) => (&token[..=idx], &token[idx + 1..]),
        None => ("", token),
    };

    let target_dir = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(dir_part)
    };

    let read_dir = match std::fs::read_dir(&target_dir) {
        Ok(rd) => rd,
        Err(_) => return candidates,
    };

    let mut entries: Vec<(String, bool)> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files unless prefix explicitly starts with '.'
        if name.starts_with('.') && !file_prefix.starts_with('.') {
            continue;
        }
        if name.starts_with(file_prefix) {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            entries.push((name, is_dir));
        }
    }

    // Sort: directories first or alphabetical
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, is_dir) in entries {
        let suffix = if is_dir { "/" } else { "" };
        let full_name = format!("{dir_part}{name}{suffix}");
        candidates.push(Candidate {
            display: full_name.clone(),
            replacement: full_name,
        });
        if candidates.len() >= MAX_COMPLETIONS {
            break;
        }
    }

    candidates
}
