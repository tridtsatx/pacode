//! `edit`: exact string replacement.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{first_changed_line, is_outside_workspace, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "edit";

pub struct EditTool;

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
        "Replace an exact string in a file. `old_string` must occur exactly once \
         (include surrounding lines to make it unique) unless `replace_all` is true. \
         Read the file first so the match is exact."
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

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Edit <path>`, detail = unified diff (context 3); atomic write;
    /// output `Edited <path>: N replacement(s)`, diff stat; preview = first changed line.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<EditInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.new_string.contains('\0') || args.old_string.contains('\0') {
            return Err(ToolError::invalid("edit strings cannot contain NUL byte"));
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
        let content = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;

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

        let diff = codeapp_render::unified_diff(&content, &new_content, &rel_path, 3);
        let stat = codeapp_render::diff_stat(&content, &new_content);

        let outside = is_outside_workspace(&full_path, &ctx.cwd);
        let perm_title = if outside {
            format!("Edit {rel_path} (outside workspace)")
        } else {
            format!("Edit {rel_path}")
        };

        ctx.require_permission(perm_title, diff.clone(), None)
            .await?;

        let tmp_path = format!("{}.codeapp-tmp", full_path.to_string_lossy());
        tokio::fs::write(&tmp_path, new_content.as_bytes()).await?;
        tokio::fs::rename(&tmp_path, &full_path).await?;

        let preview = first_changed_line(&diff);
        let title = format!("Edit {rel_path}");
        let text = format!("Edited {rel_path}: {count} replacement(s)");

        Ok(ToolOutput::text(text)
            .with_title(title)
            .with_preview(preview)
            .with_diff(stat))
    }
}
