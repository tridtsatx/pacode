//! `write`: create or overwrite a file.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{is_outside_workspace, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

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
        "Create or overwrite a file with the given content. Parent directories are \
         created. Prefer `edit` for changes to existing files."
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
    /// `codeapp_render::diff_stat`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<WriteInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        if args.content.contains('\0') {
            return Err(ToolError::invalid("content contains NUL byte"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));

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

        let rel_path = full_path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();

        let (detail, diff_stat) = match &old_content {
            Some(old) => {
                let diff = codeapp_render::unified_diff(old, &args.content, &rel_path, 3);
                let stat = codeapp_render::diff_stat(old, &args.content);
                (diff, Some(stat))
            }
            None => {
                let line_count = args.content.lines().count();
                let detail = format!("new file, {line_count} lines");
                let stat = codeapp_render::diff_stat("", &args.content);
                (detail, Some(stat))
            }
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

        let tmp_path = format!("{}.codeapp-tmp", full_path.to_string_lossy());
        tokio::fs::write(&tmp_path, args.content.as_bytes()).await?;
        tokio::fs::rename(&tmp_path, &full_path).await?;

        let line_count = args.content.lines().count();
        let content_msg = format!("Wrote {} bytes to {rel_path}", args.content.len());
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
