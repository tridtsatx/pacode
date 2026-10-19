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

/// Cap on lines of fresh anchored context appended to a successful hashline edit.
const REPORT_CONTEXT_LINES: usize = 40;

/// Path shown to the user and model: relative to `cwd` when inside it.
pub fn rel_path(full: &Path, cwd: &Path) -> String {
    full.strip_prefix(cwd)
        .unwrap_or(full)
        .to_string_lossy()
        .to_string()
}

/// Read a text file for editing: refuses non-regular, too-large (over
/// [`crate::registry::MAX_SOURCE_FILE_BYTES`]), binary (NUL in the first 8 KiB) and
/// non-UTF-8 files. `shown` is the path as the model wrote it.
pub async fn read_text_file(full: &Path, shown: &str) -> Result<String, ToolError> {
    crate::registry::read_source_file(full, crate::registry::MAX_SOURCE_FILE_BYTES)
        .await
        .map_err(|err| match err {
            crate::registry::SourceReadError::Io(io) => ToolError::Io(io),
            other => ToolError::failed(other.message("edit", shown)),
        })
}

/// Permission title `<verb> <path>` with the standard outside-workspace suffix.
pub fn permission_title(verb: &str, rel: &str, outside: bool) -> String {
    if outside {
        format!("{verb} {rel} (outside workspace)")
    } else {
        format!("{verb} {rel}")
    }
}

/// Atomic write ([`crate::registry::write_atomic`]: hidden temp file beside the
/// destination, original mode carried over, then rename). Writes through a symlink to
/// its target instead of replacing the link, and invalidates the walk cache.
pub async fn write_atomic(full: &Path, content: &str) -> Result<(), ToolError> {
    let dest = tokio::fs::canonicalize(full)
        .await
        .unwrap_or_else(|_| full.to_path_buf());
    crate::registry::write_atomic(&dest, content.as_bytes()).await?;
    crate::walk::invalidate_path(&dest);
    Ok(())
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
