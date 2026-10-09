use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::{ACCEPT_LARGE_OUTPUT_KEY, INTENT_KEY, ToolError};

/// Strip `intent` and `accept_large_output` and deserialise tool arguments into `T`.
pub fn parse_input<T: DeserializeOwned>(mut input: Value) -> Result<(T, bool), ToolError> {
    let accept_large_output = input
        .get(ACCEPT_LARGE_OUTPUT_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Value::Object(ref mut map) = input {
        map.remove(INTENT_KEY);
        map.remove(ACCEPT_LARGE_OUTPUT_KEY);
    }

    let parsed: T =
        serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
    Ok((parsed, accept_large_output))
}

/// Cap model-visible output to `cap` characters unless `accept_large` is true.
pub fn cap_output(text: &str, accept_large: bool, cap: usize) -> String {
    if accept_large {
        text.to_string()
    } else {
        pacode_types::truncate_head_tail(text, cap)
    }
}

/// The transcript-cell body: real output, head+tail-capped at
/// `PREVIEW_CAP_CHARS` — enough for the cell's collapsed view and for
/// `ctrl+o` expansion, bounded so the transcript stays light.
pub fn preview_body(text: &str) -> String {
    pacode_types::truncate_head_tail(text, pacode_types::PREVIEW_CAP_CHARS)
}

/// The new text of a whole-file change with its unified diff (context 3)
/// and line counts.
pub(crate) struct FileChange {
    pub new: String,
    pub diff: String,
    pub stat: pacode_types::DiffStat,
}

/// Diff `old` → `new` on the blocking pool. A large rewrite is real CPU work
/// (bounded by `pacode_render::DIFF_TIMEOUT`, plus tokenising up to the
/// whole-file limit) that must not stall a runtime worker the daemon's event
/// loop shares; the new text moves in and comes back so nothing is copied.
pub(crate) async fn diff_off_runtime(
    old: String,
    new: String,
    rel_path: String,
) -> Result<FileChange, ToolError> {
    tokio::task::spawn_blocking(move || {
        let (diff, stat) = pacode_render::unified_diff(&old, &new, &rel_path, 3);
        FileChange { new, diff, stat }
    })
    .await
    .map_err(|err| ToolError::failed(format!("diff task failed: {err}")))
}

/// Check if a path is outside the workspace directory `cwd`.
pub fn is_outside_workspace(path: &Path, cwd: &Path) -> bool {
    let canonical_cwd = match cwd.canonicalize() {
        Ok(c) => c,
        Err(_) => cwd.to_path_buf(),
    };
    let canonical_path = canonicalize_best_effort(path);
    !canonical_path.starts_with(&canonical_cwd)
}

/// Canonical path when it exists; otherwise canonicalizes the deepest existing
/// ancestor and re-appends the missing tail.
pub(crate) fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut stack = Vec::new();
    let mut curr = path;
    while !curr.exists() {
        if let Some(parent) = curr.parent() {
            if let Some(name) = curr.file_name() {
                stack.push(name);
            }
            curr = parent;
        } else {
            break;
        }
    }
    let mut base = curr.canonicalize().unwrap_or_else(|_| curr.to_path_buf());
    while let Some(segment) = stack.pop() {
        base.push(segment);
    }
    base
}
