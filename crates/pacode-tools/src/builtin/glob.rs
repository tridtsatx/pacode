//! `glob`: find files by pattern, newest first.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "glob";

/// Model-facing description; see `prompts/glob.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/glob.md");

#[derive(Default)]
pub struct GlobTool {
    definition: DefinitionCache,
}

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
        DESCRIPTION.trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
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

                let entries = crate::walk::shared().collect(
                    &root,
                    crate::walk::WalkOptions {
                        include_hidden: false,
                        max_depth: None,
                    },
                    &cancel,
                )?;

                let mut matched: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
                    .iter()
                    .filter(|entry| entry.is_file)
                    .filter(|entry| {
                        glob.is_match(&entry.rel)
                            || glob.is_match(entry.rel.file_name().unwrap_or_default())
                    })
                    .map(|entry| {
                        (
                            entry.mtime.unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                            entry.rel.clone(),
                        )
                    })
                    .collect();

                matched.sort_by_key(|b| std::cmp::Reverse(b.0));
                matched.truncate(max_results);

                let lines = matched
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
        let preview = if count == 0 {
            "no files".to_string()
        } else {
            preview_body(&joined)
        };
        let title = format!("Glob \"{}\"", args.pattern);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}
