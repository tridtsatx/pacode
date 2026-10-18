use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::hashline::{self, Applied, Op};
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

/// Cap on lines of fresh anchored context appended to a successful hashline edit.
const REPORT_CONTEXT_LINES: usize = 40;

/// Path shown to the user and model: relative to `cwd` when inside it.
pub fn rel_path(full: &Path, cwd: &Path) -> String {
    full.strip_prefix(cwd)
        .unwrap_or(full)
        .to_string_lossy()
        .to_string()
}

/// Read a text file for editing: refuses binary (NUL in the first 8 KiB) and non-UTF-8.
pub async fn read_text_file(full: &Path, shown: &str) -> Result<String, ToolError> {
    let bytes = tokio::fs::read(full).await?;
    let check_len = bytes.len().min(8192);
    if bytes[..check_len].contains(&0) {
        return Err(ToolError::failed(format!(
            "cannot edit binary file: {shown}"
        )));
    }
    String::from_utf8(bytes).map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))
}

/// Permission title `<verb> <path>` with the standard outside-workspace suffix.
pub fn permission_title(verb: &str, rel: &str, outside: bool) -> String {
    if outside {
        format!("{verb} {rel} (outside workspace)")
    } else {
        format!("{verb} {rel}")
    }
}

async fn write_then_rename(
    tmp: &str,
    dest: &Path,
    content: &str,
    perms: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    tokio::fs::write(tmp, content.as_bytes()).await?;
    if let Some(p) = perms {
        tokio::fs::set_permissions(tmp, p).await?;
    }
    tokio::fs::rename(tmp, dest).await
}

/// Atomic write: temp file beside the destination, original permissions copied, then
/// rename. Writes through a symlink to its target. The temp file is removed on failure.
pub async fn write_atomic(full: &Path, content: &str) -> Result<(), ToolError> {
    let dest = tokio::fs::canonicalize(full)
        .await
        .unwrap_or_else(|_| full.to_path_buf());
    let perms = tokio::fs::metadata(&dest)
        .await
        .ok()
        .map(|m| m.permissions());
    let tmp = format!("{}.pacode-tmp", dest.to_string_lossy());
    let result = write_then_rename(&tmp, &dest, content, perms).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result.map_err(ToolError::Io)
}

/// Model-visible summary of one applied hashline edit: counts, relocation notes, and the
/// fresh anchored lines around every change (so the next edit needs no re-read).
pub fn hashline_report(rel: &str, applied: &Applied) -> String {
    let mut out = format!(
        "Edited {rel}: {} op(s) applied (+{} -{} lines).",
        applied.changed.len(),
        applied.added,
        applied.removed
    );
    for r in &applied.relocations {
        out.push_str(&format!(
            "\nNote: op {} {} anchor line {} had moved; matched by hash at line {}.",
            r.op, r.role, r.from, r.to
        ));
    }
    out.push_str("\nUpdated lines (line numbers and anchors below are current):\n");
    out.push_str(&hashline::snippet(
        &applied.content,
        &applied.changed,
        2,
        REPORT_CONTEXT_LINES,
        false,
    ));
    out
}

/// Reject NUL bytes in any replacement text.
pub fn reject_nul_in_ops(ops: &[Op]) -> Result<(), ToolError> {
    let has_nul = |lines: &[String]| lines.iter().any(|l| l.contains('\0'));
    for (i, op) in ops.iter().enumerate() {
        let bad = match op {
            Op::Replace { lines, .. }
            | Op::InsertAfter { lines, .. }
            | Op::InsertBefore { lines, .. } => has_nul(lines),
            Op::Delete { .. } => false,
        };
        if bad {
            return Err(ToolError::invalid(format!(
                "op {} text cannot contain NUL byte",
                i + 1
            )));
        }
    }
    Ok(())
}
