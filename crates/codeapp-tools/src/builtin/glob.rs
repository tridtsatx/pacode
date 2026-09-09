//! `glob`: find files by pattern, newest first.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "glob";

pub struct GlobTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. `src/**/*.rs`), most recently \
         modified first, respecting .gitignore."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Root directory (default: working directory)."},
                "max_results": {"type": "integer", "minimum": 1, "default": 200}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `globset::Glob` matched against paths relative to the root while walking with
    /// `ignore::WalkBuilder`; collect (mtime, path); sort desc by mtime; cap; output one
    /// relative path per line; `spawn_blocking`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<GlobInput>(input)?;
        if args.pattern.is_empty() {
            return Err(ToolError::invalid("pattern cannot be empty"));
        }

        let root = args
            .path
            .as_deref()
            .map(|p| ctx.resolve(Path::new(p)))
            .unwrap_or_else(|| ctx.cwd.clone());

        let max_results = args.max_results.unwrap_or(200).max(1);
        let cancel = ctx.cancel.clone();
        let pattern = args.pattern.clone();

        let matched_paths =
            tokio::task::spawn_blocking(move || -> Result<Vec<String>, ToolError> {
                let glob = globset::GlobBuilder::new(&pattern)
                    .literal_separator(false)
                    .build()
                    .map_err(|e| ToolError::invalid(format!("invalid glob pattern: {e}")))?
                    .compile_matcher();

                let walker = ignore::WalkBuilder::new(&root)
                    .hidden(true)
                    .git_ignore(true)
                    .require_git(false)
                    .build();

                let mut entries = Vec::new();

                for entry in walker {
                    if cancel.is_cancelled() {
                        return Err(ToolError::Cancelled);
                    }
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        continue;
                    }
                    let path = entry.path();
                    let rel = path.strip_prefix(&root).unwrap_or(path);
                    if glob.is_match(rel) || glob.is_match(path.file_name().unwrap_or_default()) {
                        let mtime = entry
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        entries.push((mtime, rel.to_path_buf()));
                    }
                }

                entries.sort_by_key(|b| std::cmp::Reverse(b.0));
                entries.truncate(max_results);

                let lines = entries
                    .into_iter()
                    .map(|(_, p)| p.to_string_lossy().to_string())
                    .collect();
                Ok(lines)
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))??;

        let count = matched_paths.len();
        let joined = matched_paths.join("\n");
        let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
        let preview = format!("{count} files");
        let title = format!("Glob \"{}\"", args.pattern);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}
