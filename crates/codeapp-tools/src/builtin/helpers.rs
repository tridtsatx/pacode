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
        codeapp_types::truncate_head_tail(text, cap)
    }
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

fn canonicalize_best_effort(path: &Path) -> PathBuf {
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

/// Extract the first changed line from a unified diff for transcript preview.
pub fn first_changed_line(diff: &str) -> String {
    for line in diff.lines() {
        if (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
        {
            return line.to_string();
        }
    }
    String::new()
}
