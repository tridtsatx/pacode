//! `multi_edit`: several replacements in one file, all or nothing.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::edit;
use super::helpers::{first_changed_line, is_outside_workspace, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

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
struct MultiEditInput {
    path: String,
    edits: Vec<SingleEdit>,
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Apply several exact replacements to one file in order, atomically: if any \
         edit fails nothing is written. Same matching rules as `edit`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "edits"],
            "properties": {
                "path": {"type": "string"},
                "edits": {
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
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.edits.is_empty() {
            return Err(ToolError::invalid("edits cannot be empty"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        let bytes = tokio::fs::read(&full_path).await?;
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed(format!(
                "cannot edit binary file: {}",
                args.path
            )));
        }
        let original = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;

        let mut current = original.clone();
        let mut counts = Vec::with_capacity(args.edits.len());

        for (i, edit) in args.edits.iter().enumerate() {
            if edit.new_string.contains('\0') || edit.old_string.contains('\0') {
                return Err(ToolError::invalid(format!(
                    "edit #{} strings cannot contain NUL byte",
                    i + 1
                )));
            }
            let (next, count) = edit::apply_replacement(
                &current,
                &edit.old_string,
                &edit.new_string,
                edit.replace_all,
            )
            .map_err(|err| ToolError::invalid(format!("edit #{}: {err}", i + 1)))?;
            counts.push(count);
            current = next;
        }

        if original.ends_with('\n') && !current.ends_with('\n') {
            current.push('\n');
        }

        let rel_path = full_path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();

        let diff = pacode_render::unified_diff(&original, &current, &rel_path, 3);
        let stat = pacode_render::diff_stat(&original, &current);

        let outside = is_outside_workspace(&full_path, &ctx.cwd);
        let perm_title = if outside {
            format!("Edit {rel_path} (outside workspace)")
        } else {
            format!("Edit {rel_path}")
        };

        ctx.require_permission(perm_title, diff.clone(), None)
            .await?;

        let tmp_path = format!("{}.pacode-tmp", full_path.to_string_lossy());
        tokio::fs::write(&tmp_path, current.as_bytes()).await?;
        tokio::fs::rename(&tmp_path, &full_path).await?;

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
        let output_text = lines.join("\n");

        Ok(ToolOutput::text(output_text)
            .with_title(title)
            .with_preview(preview)
            .with_diff(stat))
    }
}
