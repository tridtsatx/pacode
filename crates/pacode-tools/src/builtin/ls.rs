//! `ls`: directory listing.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "ls";

pub struct LsTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct LsInput {
    path: Option<String>,
    all: bool,
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "List a directory's top level only (not recursive): one entry per line, \
         directories with a trailing `/`, files with their size. Hidden entries \
         only with `all`. For a recursive tree use `glob`; for code structure \
         use `symbols`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory (default: working directory)."},
                "all": {"type": "boolean", "default": false}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `tokio::fs::read_dir`; sorted directories first then files, alphabetical; cap
    /// 500 entries with a `… N more` line.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<LsInput>(input)?;
        let dir_path = args
            .path
            .as_deref()
            .map(|p| ctx.resolve(Path::new(p)))
            .unwrap_or_else(|| ctx.cwd.clone());

        let cancel = ctx.cancel.clone();
        let show_all = args.all;
        let dir_target = dir_path.clone();

        let (lines, total_entries) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), ToolError> {
                if !dir_target.exists() {
                    return Err(ToolError::invalid(format!(
                        "directory does not exist: {}",
                        dir_target.display()
                    )));
                }
                if !dir_target.is_dir() {
                    return Err(ToolError::invalid(format!(
                        "path is not a directory: {}",
                        dir_target.display()
                    )));
                }

                struct Item {
                    name: String,
                    is_dir: bool,
                    size: u64,
                }

                let entries = crate::walk::shared().collect(
                    &dir_target,
                    crate::walk::WalkOptions {
                        include_hidden: show_all,
                        max_depth: Some(1),
                    },
                    &cancel,
                )?;

                let mut items: Vec<Item> = entries
                    .iter()
                    .map(|entry| Item {
                        name: entry.rel.to_string_lossy().to_string(),
                        is_dir: entry.is_dir,
                        size: if entry.is_dir { 0 } else { entry.size },
                    })
                    .collect();

                items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.cmp(&b.name),
                });

                let total = items.len();
                let mut formatted_lines = Vec::new();
                let cap = 500;
                let take_count = items.len().min(cap);

                for item in items.iter().take(take_count) {
                    if item.is_dir {
                        formatted_lines.push(format!("{}/", item.name));
                    } else {
                        formatted_lines.push(format!("{} ({} B)", item.name, item.size));
                    }
                }

                if total > cap {
                    let remaining = total - cap;
                    formatted_lines.push(format!("… {remaining} more"));
                }

                Ok((formatted_lines, total))
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))??;

        let joined = lines.join("\n");
        let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
        let rel_display = dir_path
            .strip_prefix(&ctx.cwd)
            .unwrap_or(&dir_path)
            .to_string_lossy();
        let title = if rel_display.is_empty() || rel_display == "." {
            "ls".to_string()
        } else {
            format!("ls {rel_display}")
        };
        let preview = if total_entries == 0 {
            "empty".to_string()
        } else {
            preview_body(&joined)
        };

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}
