//! `grep`: regex search with ripgrep's crates (`ignore` + `grep-searcher`).

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "grep";

pub struct GrepTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    case_insensitive: bool,
    max_results: Option<usize>,
    context: Option<usize>,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression (Rust regex syntax), \
         respecting .gitignore. Returns `path:line:text` lines, at most `max_results`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Directory or file to search (default: working directory)."},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. `*.rs`."},
                "case_insensitive": {"type": "boolean", "default": false},
                "max_results": {"type": "integer", "minimum": 1, "default": 200},
                "context": {"type": "integer", "minimum": 0, "default": 0, "description": "Lines of context around each match."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `ignore::WalkBuilder` (hidden files skipped, .gitignore honoured, binary files
    /// skipped by `grep_searcher`'s binary detection), `grep_regex::RegexMatcher`,
    /// `Searcher` with line numbers; runs in `tokio::task::spawn_blocking`; stops at
    /// `max_results`; output capped; title `Grep "<pattern>"`, preview `<n> hits`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<GrepInput>(input)?;
        if args.pattern.is_empty() {
            return Err(ToolError::invalid("pattern cannot be empty"));
        }

        let search_root = args
            .path
            .as_deref()
            .map(|p| ctx.resolve(Path::new(p)))
            .unwrap_or_else(|| ctx.cwd.clone());

        let max_results = args.max_results.unwrap_or(200).max(1);
        let context_lines = args.context.unwrap_or(0);
        let cancel = ctx.cancel.clone();
        let cwd = ctx.cwd.clone();
        let pattern = args.pattern.clone();

        let (output_lines, hits_count) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), ToolError> {
                use grep_regex::RegexMatcherBuilder;
                use grep_searcher::{
                    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch,
                };

                let matcher = RegexMatcherBuilder::new()
                    .case_insensitive(args.case_insensitive)
                    .build(&pattern)
                    .map_err(|e| ToolError::invalid(format!("invalid regex pattern: {e}")))?;

                let mut searcher = SearcherBuilder::new()
                    .line_number(true)
                    .binary_detection(BinaryDetection::quit(0x00))
                    .before_context(context_lines)
                    .after_context(context_lines)
                    .build();

                let glob_matcher = if let Some(ref g) = args.glob {
                    let m = globset::GlobBuilder::new(g)
                        .build()
                        .map_err(|e| ToolError::invalid(format!("invalid glob: {e}")))?
                        .compile_matcher();
                    Some(m)
                } else {
                    None
                };

                let mut results = Vec::new();
                let mut total_hits = 0usize;

                struct CollectSink<'a> {
                    results: &'a mut Vec<String>,
                    display_path: &'a str,
                    total_hits: &'a mut usize,
                    max_results: usize,
                }

                impl<'a> Sink for CollectSink<'a> {
                    type Error = std::io::Error;

                    fn matched(
                        &mut self,
                        _searcher: &Searcher,
                        mat: &SinkMatch<'_>,
                    ) -> Result<bool, Self::Error> {
                        *self.total_hits += 1;
                        let line_num = mat.line_number().unwrap_or(0);
                        let line_bytes = mat.bytes();
                        let line = String::from_utf8_lossy(line_bytes);
                        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                        self.results
                            .push(format!("{}:{}:{}", self.display_path, line_num, trimmed));
                        if *self.total_hits >= self.max_results {
                            return Ok(false);
                        }
                        Ok(true)
                    }

                    fn context(
                        &mut self,
                        _searcher: &Searcher,
                        ctx: &SinkContext<'_>,
                    ) -> Result<bool, Self::Error> {
                        let line_num = ctx.line_number().unwrap_or(0);
                        let line_bytes = ctx.bytes();
                        let line = String::from_utf8_lossy(line_bytes);
                        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                        self.results
                            .push(format!("{}-{}-{}", self.display_path, line_num, trimmed));
                        Ok(true)
                    }
                }

                if search_root.is_file() {
                    let rel_path = search_root
                        .strip_prefix(&cwd)
                        .unwrap_or(&search_root)
                        .to_string_lossy()
                        .to_string();
                    let mut sink = CollectSink {
                        results: &mut results,
                        display_path: &rel_path,
                        total_hits: &mut total_hits,
                        max_results,
                    };
                    let _ = searcher.search_path(&matcher, &search_root, &mut sink);
                    return Ok((results, total_hits));
                }

                let walker = ignore::WalkBuilder::new(&search_root)
                    .hidden(true)
                    .git_ignore(true)
                    .require_git(false)
                    .build();

                for entry in walker {
                    if cancel.is_cancelled() {
                        return Err(ToolError::Cancelled);
                    }
                    if total_hits >= max_results {
                        break;
                    }
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        continue;
                    }
                    let path = entry.path();
                    if let Some(ref gm) = glob_matcher {
                        let filename = path.file_name().unwrap_or_default();
                        let rel = path.strip_prefix(&search_root).unwrap_or(path);
                        if !gm.is_match(filename) && !gm.is_match(rel) {
                            continue;
                        }
                    }
                    let display_path = path
                        .strip_prefix(&cwd)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string();

                    let mut sink = CollectSink {
                        results: &mut results,
                        display_path: &display_path,
                        total_hits: &mut total_hits,
                        max_results,
                    };
                    let _ = searcher.search_path(&matcher, path, &mut sink);
                }

                Ok((results, total_hits))
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))??;

        let joined = output_lines.join("\n");
        let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
        let preview = format!("{hits_count} hits");
        let title = format!("Grep \"{}\"", args.pattern);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}
