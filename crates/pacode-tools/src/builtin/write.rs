//! `write`: create or overwrite a file.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{
    FileChange, diff_off_runtime, is_outside_workspace, parse_input, preview_body, write_atomic,
};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WriteIntent};

pub const NAME: &str = "write";

/// A unit struct (also constructible with `default()`): tests use the plain
/// value, so it carries no `DefinitionCache`; the registry's own definitions
/// cache covers the per-step cost.
#[derive(Default)]
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
        include_str!("../prompts/write.md").trim()
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

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Write <path>` with a unified diff against the current content
    /// (or `new file, N lines`) as detail; the read of the current content and the
    /// write itself run under the path's write lock, and the write lands atomically
    /// (temp file + rename in the same dir) keeping the existing mode; output
    /// `Wrote <bytes> bytes to <path>`; diff stat from the same diff pass.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<WriteInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.content.contains('\0') {
            return Err(ToolError::invalid("content contains NUL byte"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        // Read, ledger check and write all happen under the ledger lock and the
        // path's write lock (always taken in that order).
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;
        let _write_lock = crate::registry::acquire_path_write_lock(&full_path, &ctx.cancel).await?;

        let old_content = match crate::registry::read_source_file(
            &full_path,
            crate::registry::MAX_SOURCE_FILE_BYTES,
        )
        .await
        {
            Ok(content) => Some(content),
            Err(err) if err.is_not_found() => None,
            Err(err) => {
                return Err(ToolError::failed(err.message("overwrite", &args.path)));
            }
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

        let is_new_file = old_content.is_none();
        // A new file diffs against "": the all-added diff doubles as the cell
        // body (the written content itself) and shows the reviewer what lands
        // on disk.
        let FileChange {
            new: content,
            diff,
            stat,
        } = diff_off_runtime(
            old_content.unwrap_or_default(),
            args.content,
            rel_path.clone(),
        )
        .await?;
        let extra_head =
            is_new_file.then(|| format!("new file, {} lines\n", content.lines().count()));

        let outside = is_outside_workspace(&full_path, &ctx.cwd);
        let perm_title = if outside {
            format!("Write {rel_path} (outside workspace)")
        } else {
            format!("Write {rel_path}")
        };

        let capped = crate::registry::cap_output_bytes(
            diff.as_str(),
            crate::registry::MAX_PERMISSION_DETAIL_BYTES,
        );
        let detail = match &extra_head {
            Some(head) => format!("{head}{capped}"),
            None => capped.into_owned(),
        };
        ctx.require_permission(perm_title, detail, None).await?;

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        write_atomic(&full_path, &content).await?;
        ctx.host
            .ledger_record_write(&ctx.agent, &full_path, &content)
            .await;

        let mut content_msg = format!("Wrote {} bytes to {rel_path}", content.len());
        if let Some(note) = pass.note {
            content_msg.push_str("\n\n");
            content_msg.push_str(&note);
        }
        let preview = preview_body(&diff);
        let title = format!("Write {rel_path}");

        Ok(ToolOutput::text(content_msg)
            .with_title(title)
            .with_preview(preview)
            .with_diff(stat))
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod write_tests;
