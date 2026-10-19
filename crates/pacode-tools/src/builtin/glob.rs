//! `glob`: find files by pattern, newest first.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    /// relative path per line, plus a footer naming how many matches the cap hid;
    /// `spawn_blocking`.
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

        let (matched_paths, total) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), ToolError> {
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

                // Bounded top-N by mtime (cap: `max_results` entries; owner: this call). Ties keep
                // walk order via `seq`, as a stable sort would; `total` counts every match so
                // the footer can name what the cap hid.
                let mut heap: BinaryHeap<Reverse<(SystemTime, Reverse<usize>, PathBuf)>> =
                    BinaryHeap::with_capacity(max_results.min(4096) + 1);
                let mut total = 0usize;

                for entry in entries.iter().filter(|entry| entry.is_file) {
                    if !(glob.is_match(&entry.rel)
                        || glob.is_match(entry.rel.file_name().unwrap_or_default()))
                    {
                        continue;
                    }
                    let mtime = entry.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
                    heap.push(Reverse((mtime, Reverse(total), entry.rel.clone())));
                    total += 1;
                    if heap.len() > max_results {
                        heap.pop();
                    }
                }

                // Ascending order of `Reverse(key)` is newest first.
                let lines = heap
                    .into_sorted_vec()
                    .into_iter()
                    .map(|Reverse((_, _, p))| p.to_string_lossy().to_string())
                    .collect();
                Ok((lines, total))
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))??;

        let count = matched_paths.len();
        let mut joined = matched_paths.join("\n");
        if total > count {
            let more = total - count;
            joined.push_str(&format!(
                "\n— {more} more matching files not shown (newest {count} listed); narrow the \
                 pattern or raise `max_results`"
            ));
        }
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

#[cfg(test)]
#[path = "glob_tests.rs"]
mod glob_tests;
