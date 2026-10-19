//! `write`: create or overwrite a file.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{is_outside_workspace, parse_input, write_atomic};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WriteIntent};

pub const NAME: &str = "write";

pub struct WriteTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct WriteInput {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        // Formatted once from constant text (not a cache: nothing to invalidate).
        static DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            format!(
                "Create or overwrite a file with the given content. Parent directories are \
                 created. Prefer `edit` for changes to existing files: a full overwrite is \
                 rejected when it would revert lines another agent changed after your read. \
                 New files need no prior read. {}",
                super::edit::CONCURRENCY_NOTE
            )
        });
        DESCRIPTION.as_str()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Write <path>` with a unified diff against the current content
    /// (or `new file, N lines`) as detail; writes atomically (temp file + rename in the
    /// same dir); output `Wrote <bytes> bytes to <path>`; diff stat from
    /// `pacode_render::diff_stat`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (mut args, _accept_large_output) = parse_input::<WriteInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.content.contains('\0') {
            return Err(ToolError::invalid("content contains NUL byte"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        // Read, ledger check and write all happen under the path's write lock.
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;

        let old_content = match tokio::fs::read(&full_path).await {
            Ok(bytes) => {
                let check_len = bytes.len().min(8192);
                if bytes[..check_len].contains(&0) {
                    return Err(ToolError::failed(format!(
                        "cannot overwrite binary file: {}",
                        args.path
                    )));
                }
                Some(String::from_utf8(bytes).map_err(|e| {
                    ToolError::failed(format!("existing file is not valid UTF-8: {e}"))
                })?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(ToolError::Io(e)),
        };
        let pass = ctx
            .host
            .ledger_check(
                &ctx.agent,
                &full_path,
                WriteIntent::Write,
                old_content.as_deref(),
                Some(args.content.as_str()),
            )
            .await?;

        let rel_path = full_path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();

        // Diff and stat are CPU work proportional to the file: compute both in one
        // `spawn_blocking`; the new content moves in and back out (no copy).
        let (detail, diff_stat) = {
            let new_content = std::mem::take(&mut args.content);
            let rel = rel_path.clone();
            let (detail, stat, new_content) = tokio::task::spawn_blocking(move || {
                let (detail, stat) = match &old_content {
                    Some(old) => (
                        pacode_render::unified_diff(old, &new_content, &rel, 3),
                        pacode_render::diff_stat(old, &new_content),
                    ),
                    None => (
                        format!("new file, {} lines", new_content.lines().count()),
                        pacode_render::diff_stat("", &new_content),
                    ),
                };
                (detail, stat, new_content)
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;
            args.content = new_content;
            (detail, Some(stat))
        };

        let outside = is_outside_workspace(&full_path, &ctx.cwd);
        let perm_title = if outside {
            format!("Write {rel_path} (outside workspace)")
        } else {
            format!("Write {rel_path}")
        };

        ctx.require_permission(perm_title, detail, None).await?;

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        write_atomic(&full_path, &args.content).await?;
        ctx.host
            .ledger_record_write(&ctx.agent, &full_path, &args.content)
            .await;

        let line_count = args.content.lines().count();
        let mut content_msg = format!("Wrote {} bytes to {rel_path}", args.content.len());
        if let Some(note) = pass.note {
            content_msg.push_str("\n\n");
            content_msg.push_str(&note);
        }
        let preview = format!("{line_count} lines");
        let title = format!("Write {rel_path}");

        let mut out = ToolOutput::text(content_msg)
            .with_title(title)
            .with_preview(preview);
        if let Some(stat) = diff_stat {
            out = out.with_diff(stat);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod write_tests;
