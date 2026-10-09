//! `edit`: exact string replacement.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{
    FileChange, diff_off_runtime, is_outside_workspace, parse_input, preview_body,
};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "edit";

/// Model-facing description; see `prompts/edit.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/edit.md");

#[derive(Default)]
pub struct EditTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EditInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
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
        DESCRIPTION.trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean", "default": false}
            }
        })
    }

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Edit <path>`, detail = unified diff (context 3); the whole
    /// read-modify-write runs under the path's write lock and the write lands
    /// atomically; output `Edited <path>: N replacement(s)`, diff stat; preview = the diff.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<EditInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.new_string.contains('\0') || args.old_string.contains('\0') {
            return Err(ToolError::invalid("edit strings cannot contain NUL byte"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        let _write_lock = crate::registry::acquire_path_write_lock(&full_path, &ctx.cancel).await?;
        let content =
            crate::registry::read_source_file(&full_path, crate::registry::MAX_SOURCE_FILE_BYTES)
                .await
                .map_err(|err| ToolError::failed(err.message("edit", &args.path)))?;

        let (mut new_content, count) = apply_replacement(
            &content,
            &args.old_string,
            &args.new_string,
            args.replace_all,
        )
        .map_err(ToolError::invalid)?;

        // preserve a trailing newline if original content had one
        if content.ends_with('\n') && !new_content.ends_with('\n') {
            new_content.push('\n');
        }

        let rel_path = full_path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();

        let FileChange {
            new: new_content,
            diff,
            stat,
        } = diff_off_runtime(content, new_content, rel_path.clone()).await?;

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

        crate::registry::write_atomic(&full_path, new_content.as_bytes()).await?;
        crate::walk::invalidate_path(&full_path);

        let preview = preview_body(&diff);
        let title = format!("Edit {rel_path}");
        let text = format!("Edited {rel_path}: {count} replacement(s)");

        Ok(ToolOutput::text(text)
            .with_title(title)
            .with_preview(preview)
            .with_diff(stat))
    }
}
