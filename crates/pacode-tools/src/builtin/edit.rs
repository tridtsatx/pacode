//! `edit`: hashline edits (anchors from `read`), with exact string replacement as a
//! fallback input form.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{
    first_changed_line, hashline_report, is_outside_workspace, parse_input, permission_title,
    read_text_file, reject_nul_in_ops, rel_path, write_atomic,
};
use crate::hashline::{Applied, Op, apply_ops};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WriteIntent};

/// Shared by `edit`, `multi_edit` and `write` descriptions: how concurrent agents and
/// ledger rejections work.
pub(crate) const CONCURRENCY_NOTE: &str = "Other agents may be editing the same files \
     concurrently. You must `read` an existing file before changing it. A change that \
     would overwrite lines someone else modified after your read is rejected; the \
     rejection names who changed which lines and shows them with fresh anchors, which \
     count as read, so retry directly (or re-read only the indicated line ranges, never \
     the whole file). Changes elsewhere in the file are reported but do not block you.";

/// An edit that could not be applied (stale anchor, `old_string` not found) may have
/// failed because another agent changed the file: when the ledger says so, lead with
/// who changed which lines and the fresh lines; otherwise return `err` unchanged.
pub(crate) async fn explain_failed_edit(
    ctx: &ToolCtx,
    full_path: &Path,
    current: &str,
    intent: WriteIntent,
    err: String,
) -> String {
    match ctx
        .host
        .ledger_check(&ctx.agent, full_path, intent, Some(current), None)
        .await
    {
        Ok(_) => err,
        Err(ToolError::Failed(ledger)) => format!("{ledger}\n\n{err}"),
        Err(other) => format!("{other}\n\n{err}"),
    }
}

pub const NAME: &str = "edit";

pub struct EditTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct EditInput {
    path: String,
    /// Hashline form (preferred).
    ops: Vec<Op>,
    /// Legacy exact-string form.
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// How the new content was produced, for the model-visible summary.
enum Outcome {
    Hashline(Applied),
    Legacy(usize),
}

/// Apply one replacement to `content`. Errors: `old_string` not found, or found more
/// than once without `replace_all` (message says how many matches). Returns the new
/// content and the number of replacements.
pub fn apply_replacement(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old.is_empty() {
        return Err("old_string cannot be empty".to_string());
    }
    let count = content.matches(old).count();
    if count == 0 {
        return Err(format!("old_string not found in content: {old:?}"));
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old_string occurs {count} times (must be unique without replace_all; provide more surrounding lines)"
        ));
    }
    if replace_all {
        let new_content = content.replace(old, new);
        Ok((new_content, count))
    } else {
        let idx = content
            .find(old)
            .ok_or_else(|| "old_string not found".to_string())?;
        let mut new_content = String::with_capacity(content.len() + new.len() - old.len());
        new_content.push_str(&content[..idx]);
        new_content.push_str(new);
        new_content.push_str(&content[idx + old.len()..]);
        Ok((new_content, 1))
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        // Formatted once from constant text (not a cache: nothing to invalidate).
        static DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            format!(
                "Edit a file by line anchors. `read` shows each line as `LINE#HASH| text`; pass \
                 those anchors (`12#ab`) in `ops` instead of retyping unchanged text. Ops: \
                 `replace` {{start, end?, lines}} (inclusive range), `insert_after` / \
                 `insert_before` {{at, lines}} (`at` may be `BOF` or `EOF`), `delete` \
                 {{start, end?}}. `lines` is the new text, one array item per line, without \
                 anchors. All anchors refer to the file as you last read it; ops must not \
                 overlap; list them in any order. If an anchor is stale the whole edit is \
                 rejected and the current anchored lines are returned: retry with those. Small \
                 line drift is corrected automatically. Fallback form (avoid): \
                 `old_string`/`new_string`/`replace_all` exact replacement. {CONCURRENCY_NOTE}"
            )
        });
        DESCRIPTION.as_str()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "ops": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Hashline operations (preferred).",
                    "items": {
                        "type": "object",
                        "required": ["op"],
                        "properties": {
                            "op": {"type": "string", "enum": ["replace", "insert_after", "insert_before", "delete"]},
                            "start": {"type": "string", "description": "First line anchor, e.g. `12#ab` (replace, delete)."},
                            "end": {"type": "string", "description": "Last line anchor, inclusive; defaults to start."},
                            "at": {"type": "string", "description": "Anchor to insert next to; `BOF`/`EOF` allowed."},
                            "lines": {"type": "array", "items": {"type": "string"}, "description": "New text, one item per line, no anchors."}
                        }
                    }
                },
                "old_string": {"type": "string", "description": "Fallback: exact text to replace (use `ops` instead)."},
                "new_string": {"type": "string", "description": "Fallback: replacement text."},
                "replace_all": {"type": "boolean", "default": false, "description": "Fallback: replace every occurrence."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Edit <path>`, detail = unified diff (context 3); atomic write;
    /// output lists the changes and fresh anchors (or `N replacement(s)` for the legacy
    /// form); preview = first changed line.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<EditInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        let legacy = !args.old_string.is_empty();
        match (args.ops.is_empty(), legacy) {
            (false, true) => {
                return Err(ToolError::invalid(
                    "use either `ops` or old_string/new_string, not both",
                ));
            }
            (true, false) => {
                return Err(ToolError::invalid(
                    "provide `ops` (preferred) or old_string/new_string",
                ));
            }
            _ => {}
        }
        if args.new_string.contains('\0') || args.old_string.contains('\0') {
            return Err(ToolError::invalid("edit strings cannot contain NUL byte"));
        }
        reject_nul_in_ops(&args.ops)?;

        let full_path = ctx.resolve(Path::new(&args.path));
        // Read, ledger check and write all happen under the path's write lock.
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;
        let content = read_text_file(&full_path, &args.path).await?;
        let intent = if legacy {
            WriteIntent::Edit
        } else {
            WriteIntent::Hashline
        };

        let (new_content, outcome) = if legacy {
            let replaced = apply_replacement(
                &content,
                &args.old_string,
                &args.new_string,
                args.replace_all,
            );
            let (mut new_content, count) = match replaced {
                Ok(r) => r,
                Err(err) => {
                    let why = explain_failed_edit(ctx, &full_path, &content, intent, err).await;
                    return Err(ToolError::invalid(why));
                }
            };
            // preserve a trailing newline if original content had one
            if content.ends_with('\n') && !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            (new_content, Outcome::Legacy(count))
        } else {
            let mut applied = match apply_ops(&content, &args.ops) {
                Ok(a) => a,
                Err(err) => {
                    let why =
                        explain_failed_edit(ctx, &full_path, &content, intent, err.to_string())
                            .await;
                    return Err(ToolError::invalid(why));
                }
            };
            let new_content = std::mem::take(&mut applied.content);
            (new_content, Outcome::Hashline(applied))
        };
        let pass = ctx
            .host
            .ledger_check(
                &ctx.agent,
                &full_path,
                intent,
                Some(content.as_str()),
                Some(new_content.as_str()),
            )
            .await?;

        let rel = rel_path(&full_path, &ctx.cwd);
        let outside = is_outside_workspace(&full_path, &ctx.cwd);

        // Diff, stat and the hashline report are CPU work proportional to the file:
        // compute them together off the async worker. The contents move in and back out.
        let (diff, stat, report, new_content) = {
            let rel = rel.clone();
            tokio::task::spawn_blocking(move || {
                let mut new_content = new_content;
                let diff = pacode_render::unified_diff(&content, &new_content, &rel, 3);
                let stat = pacode_render::diff_stat(&content, &new_content);
                let report = match outcome {
                    Outcome::Legacy(count) => format!("Edited {rel}: {count} replacement(s)"),
                    Outcome::Hashline(mut applied) => {
                        applied.content = std::mem::take(&mut new_content);
                        let report = hashline_report(&rel, &applied);
                        new_content = std::mem::take(&mut applied.content);
                        report
                    }
                };
                (diff, stat, report, new_content)
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?
        };

        ctx.require_permission(permission_title("Edit", &rel, outside), diff.clone(), None)
            .await?;
        write_atomic(&full_path, &new_content).await?;
        ctx.host
            .ledger_record_write(&ctx.agent, &full_path, &new_content)
            .await;

        let mut text = report;
        if let Some(note) = pass.note {
            text.push_str("\n\n");
            text.push_str(&note);
        }
        Ok(ToolOutput::text(text)
            .with_title(format!("Edit {rel}"))
            .with_preview(first_changed_line(&diff))
            .with_diff(stat))
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod edit_tests;
