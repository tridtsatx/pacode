//! `multi_edit`: several replacements in one file, all or nothing.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::edit;
use super::helpers::{is_outside_workspace, parse_input, preview_body};
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

    /// Applies `edit::apply_replacement` sequentially in memory under the path's
    /// write lock; one permission request with the combined diff; the write lands
    /// atomically; output lists replacements per edit.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<MultiEditInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.edits.is_empty() {
            return Err(ToolError::invalid("edits cannot be empty"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        let _write_lock = crate::registry::acquire_path_write_lock(&full_path, &ctx.cancel).await?;
        let original =
            crate::registry::read_source_file(&full_path, crate::registry::MAX_SOURCE_FILE_BYTES)
                .await
                .map_err(|err| ToolError::failed(err.message("edit", &args.path)))?;

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

        let detail = crate::registry::cap_output_bytes(
            diff.as_str(),
            crate::registry::MAX_PERMISSION_DETAIL_BYTES,
        )
        .into_owned();
        ctx.require_permission(perm_title, detail, None).await?;

        crate::registry::write_atomic(&full_path, current.as_bytes()).await?;
        crate::walk::invalidate_path(&full_path);

        let preview = preview_body(&diff);
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
