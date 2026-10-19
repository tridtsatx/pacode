//! `multi_edit`: several edits, all or nothing. Preferred form: hashline `ops` across
//! several files (`files`); fallback: exact replacements in one file (`path` + `edits`).

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::edit;
use super::helpers::{
    first_changed_line, hashline_report, is_outside_workspace, parse_input, permission_title,
    read_text_file, reject_nul_in_ops, rel_path, write_atomic,
};
use crate::hashline::{Applied, Op, apply_ops};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WriteIntent};

pub const NAME: &str = "multi_edit";

pub struct MultiEditTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct SingleEdit {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileOps {
    path: String,
    ops: Vec<Op>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MultiEditInput {
    /// Hashline form: ops per file.
    files: Vec<FileOps>,
    /// Legacy form: one file, exact replacements.
    path: String,
    edits: Vec<SingleEdit>,
}

/// One file's validated change, ready to write.
struct Planned {
    full_path: std::path::PathBuf,
    rel: String,
    outside: bool,
    original: String,
    applied: Applied,
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        // Formatted once from constant text (not a cache: nothing to invalidate).
        static DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            format!(
                "Apply hashline edits to several files at once, atomically: `files` = \
                 [{{path, ops}}] with the same `ops` and `LINE#HASH` anchors as `edit`. Every \
                 anchor in every file is verified first; if any is stale nothing is written and \
                 the current anchored lines are returned. Fallback form (avoid): `path` + \
                 `edits` [{{old_string, new_string, replace_all}}] applied in order to one file. \
                 {}",
                edit::CONCURRENCY_NOTE
            )
        });
        DESCRIPTION.as_str()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Hashline form (preferred): one entry per file, each path once.",
                    "items": {
                        "type": "object",
                        "required": ["path", "ops"],
                        "properties": {
                            "path": {"type": "string"},
                            "ops": {
                                "type": "array",
                                "minItems": 1,
                                "description": "Same op objects as the `edit` tool: replace {start, end?, lines}, insert_after / insert_before {at, lines}, delete {start, end?}.",
                                "items": {
                                    "type": "object",
                                    "required": ["op"],
                                    "properties": {
                                        "op": {"type": "string", "enum": ["replace", "insert_after", "insert_before", "delete"]},
                                        "start": {"type": "string"},
                                        "end": {"type": "string"},
                                        "at": {"type": "string"},
                                        "lines": {"type": "array", "items": {"type": "string"}}
                                    }
                                }
                            }
                        }
                    }
                },
                "path": {"type": "string", "description": "Fallback form: the single file."},
                "edits": {
                    "description": "Fallback form: exact replacements, applied in order.",
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": {"type": "string"},
                            "new_string": {"type": "string"},
                            "replace_all": {"type": "boolean", "default": false}
                        }
                    }
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Applies `edit::apply_replacement` sequentially in memory; one permission request
    /// with the combined diff; atomic write; output lists replacements per edit.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<MultiEditInput>(input)?;
        if !args.files.is_empty() {
            if !args.path.is_empty() || !args.edits.is_empty() {
                return Err(ToolError::invalid(
                    "use either `files` or path/edits, not both",
                ));
            }
            return call_hashline(args.files, ctx).await;
        }
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.edits.is_empty() {
            return Err(ToolError::invalid("edits cannot be empty"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        // Read, ledger check and write all happen under the path's write lock.
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;
        let original = read_text_file(&full_path, &args.path).await?;

        let mut current = original.clone();
        let mut counts = Vec::with_capacity(args.edits.len());

        for (i, edit) in args.edits.iter().enumerate() {
            if edit.new_string.contains('\0') || edit.old_string.contains('\0') {
                return Err(ToolError::invalid(format!(
                    "edit #{} strings cannot contain NUL byte",
                    i + 1
                )));
            }
            let replaced = edit::apply_replacement(
                &current,
                &edit.old_string,
                &edit.new_string,
                edit.replace_all,
            );
            let (next, count) = match replaced {
                Ok(r) => r,
                Err(err) => {
                    let err = format!("edit #{}: {err}", i + 1);
                    let why = edit::explain_failed_edit(
                        ctx,
                        &full_path,
                        &original,
                        WriteIntent::Edit,
                        err,
                    )
                    .await;
                    return Err(ToolError::invalid(why));
                }
            };
            counts.push(count);
            current = next;
        }

        if original.ends_with('\n') && !current.ends_with('\n') {
            current.push('\n');
        }
        let pass = ctx
            .host
            .ledger_check(
                &ctx.agent,
                &full_path,
                WriteIntent::Edit,
                Some(original.as_str()),
                Some(current.as_str()),
            )
            .await?;

        let rel_path = rel_path(&full_path, &ctx.cwd);

        // Diff and stat in one `spawn_blocking`; `current` moves in and back out.
        let (diff, stat, current) = {
            let rel = rel_path.clone();
            tokio::task::spawn_blocking(move || {
                let diff = pacode_render::unified_diff(&original, &current, &rel, 3);
                let stat = pacode_render::diff_stat(&original, &current);
                (diff, stat, current)
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?
        };

        let outside = is_outside_workspace(&full_path, &ctx.cwd);
        let perm_title = permission_title("Edit", &rel_path, outside);

        ctx.require_permission(perm_title, diff.clone(), None)
            .await?;

        write_atomic(&full_path, &current).await?;
        ctx.host
            .ledger_record_write(&ctx.agent, &full_path, &current)
            .await;

        let preview = first_changed_line(&diff);
        let title = format!("Multi-edit {rel_path}");

        let mut lines = Vec::new();
        lines.push(format!(
            "Multi-edited {rel_path} ({} edits):",
            args.edits.len()
        ));
        for (idx, count) in counts.iter().enumerate() {
            lines.push(format!("  Edit #{}: {count} replacement(s)", idx + 1));
        }
        if let Some(note) = pass.note {
            lines.push(String::new());
            lines.push(note);
        }
        let output_text = lines.join("\n");

        Ok(ToolOutput::text(output_text)
            .with_title(title)
            .with_preview(preview)
            .with_diff(stat))
    }
}

/// Hashline form: validate every file first (collecting all stale anchors), ask once,
/// then write; a failed write restores the files already written.
async fn call_hashline(files: Vec<FileOps>, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
    let mut seen: Vec<std::path::PathBuf> = Vec::with_capacity(files.len());
    for file in &files {
        if file.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if file.ops.is_empty() {
            return Err(ToolError::invalid(format!(
                "{}: ops cannot be empty",
                file.path
            )));
        }
        reject_nul_in_ops(&file.ops)?;
        let full_path = ctx.resolve(Path::new(&file.path));
        if seen.contains(&full_path) {
            return Err(ToolError::invalid(format!(
                "{} listed twice; put all its ops in one entry",
                file.path
            )));
        }
        seen.push(full_path);
    }
    // Every file is read, checked and written under its write lock (taken together,
    // in the host's fixed order, so two multi-file edits cannot deadlock).
    let _lock = ctx.host.ledger_lock(&seen).await;

    let mut planned: Vec<Planned> = Vec::with_capacity(files.len());
    let mut problems: Vec<String> = Vec::new();
    for (file, full_path) in files.iter().zip(seen.iter()) {
        let original = read_text_file(full_path, &file.path).await?;
        match apply_ops(&original, &file.ops) {
            Ok(applied) => planned.push(Planned {
                rel: rel_path(full_path, &ctx.cwd),
                outside: is_outside_workspace(full_path, &ctx.cwd),
                full_path: full_path.clone(),
                original,
                applied,
            }),
            Err(err) => {
                let why = edit::explain_failed_edit(
                    ctx,
                    full_path,
                    &original,
                    WriteIntent::Hashline,
                    err.to_string(),
                )
                .await;
                problems.push(format!("{}: {why}", file.path));
            }
        }
    }
    let mut notes: Vec<String> = Vec::new();
    if problems.is_empty() {
        for p in &planned {
            let checked = ctx
                .host
                .ledger_check(
                    &ctx.agent,
                    &p.full_path,
                    WriteIntent::Hashline,
                    Some(p.original.as_str()),
                    Some(p.applied.content.as_str()),
                )
                .await;
            match checked {
                Ok(pass) => notes.extend(pass.note),
                Err(ToolError::Failed(why)) => problems.push(format!("{}: {why}", p.rel)),
                Err(other) => return Err(other),
            }
        }
    }
    if !problems.is_empty() {
        return Err(ToolError::invalid(format!(
            "{}\nNo file was written.",
            problems.join("\n\n")
        )));
    }

    // All files' diffs and stats in one `spawn_blocking`; `planned` moves in and back out.
    let (planned, detail, added, removed) = tokio::task::spawn_blocking(move || {
        let mut detail = String::new();
        let (mut added, mut removed) = (0u32, 0u32);
        for p in &planned {
            detail.push_str(&pacode_render::unified_diff(
                &p.original,
                &p.applied.content,
                &p.rel,
                3,
            ));
            let stat = pacode_render::diff_stat(&p.original, &p.applied.content);
            added = added.saturating_add(stat.added);
            removed = removed.saturating_add(stat.removed);
        }
        (planned, detail, added, removed)
    })
    .await
    .map_err(|e| ToolError::failed(e.to_string()))?;
    let any_outside = planned.iter().any(|p| p.outside);
    let names: Vec<&str> = planned.iter().map(|p| p.rel.as_str()).collect();
    let subject = match names.as_slice() {
        [one] => (*one).to_string(),
        many => format!("{} files ({})", many.len(), many.join(", ")),
    };
    ctx.require_permission(
        permission_title("Edit", &subject, any_outside),
        detail.clone(),
        None,
    )
    .await?;

    for (i, p) in planned.iter().enumerate() {
        if let Err(err) = write_atomic(&p.full_path, &p.applied.content).await {
            for done in planned.iter().take(i) {
                let _ = write_atomic(&done.full_path, &done.original).await;
            }
            return Err(ToolError::failed(format!(
                "writing {} failed ({err}); earlier files were restored",
                p.rel
            )));
        }
    }

    for p in &planned {
        ctx.host
            .ledger_record_write(&ctx.agent, &p.full_path, &p.applied.content)
            .await;
    }

    let mut sections = vec![format!("Multi-edited {} file(s):", planned.len())];
    sections.extend(planned.iter().map(|p| hashline_report(&p.rel, &p.applied)));
    sections.extend(notes);
    Ok(ToolOutput::text(sections.join("\n\n"))
        .with_title(format!("Multi-edit {subject}"))
        .with_preview(first_changed_line(&detail))
        .with_diff(pacode_types::DiffStat { added, removed }))
}

#[cfg(test)]
#[path = "multi_edit_tests.rs"]
mod multi_edit_tests;
