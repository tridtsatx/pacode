//! `grep`: regex search with ripgrep's crates (`ignore` + `grep-searcher`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "grep";

/// Model-facing description; see `prompts/grep.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/grep.md");

const DEFAULT_MAX_RESULTS: usize = 200;

/// Longest line (bytes) a hit or context row carries. A minified bundle or a
/// lockfile can put 100 KB on one line; one such hit would eat the whole
/// output cap and push every other hit out of the head+tail cut. Past this the
/// row keeps a window starting just before the first match and names the full
/// length, so the model still sees where and what matched and can `read` it.
const MAX_LINE_BYTES: usize = 400;

#[derive(Default)]
pub struct GrepTool {
    definition: DefinitionCache,
}

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
                "path": {"type": "string", "description": "Directory or file to search (default: working directory)."},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. `*.rs`."},
                "case_insensitive": {"type": "boolean", "default": false},
                "max_results": {"type": "integer", "minimum": 1, "default": DEFAULT_MAX_RESULTS},
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
    /// `max_results` and says so; long lines clipped around the match; output
    /// capped; title `Grep "<pattern>"`, preview = the hits.
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
        let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
        let title = format!("Grep \"{}\"", args.pattern);
        let query = Query {
            matcher: grep_regex::RegexMatcherBuilder::new()
                .case_insensitive(args.case_insensitive)
                .build(&args.pattern)
                .map_err(|e| ToolError::invalid(format!("invalid regex pattern: {e}")))?,
            glob: args
                .glob
                .as_deref()
                .map(|g| {
                    globset::GlobBuilder::new(g)
                        .build()
                        .map(|glob| glob.compile_matcher())
                        .map_err(|e| ToolError::invalid(format!("invalid glob: {e}")))
                })
                .transpose()?,
            context_lines: args.context.unwrap_or(0),
            max_results,
        };
        let cancel = ctx.cancel.clone();
        let cwd = ctx.cwd.clone();

        let (output_lines, hits_count) =
            tokio::task::spawn_blocking(move || search(&query, search_root, &cwd, &cancel))
                .await
                .map_err(|e| ToolError::failed(e.to_string()))??;

        let mut joined = output_lines.join("\n");
        // The search stops at the limit, so whether more matches exist is
        // unknown — but the model must know the list may be partial.
        if hits_count >= max_results {
            joined.push_str(&format!(
                "\n— stopped at max_results={max_results}; more matches may exist, narrow \
                 `path`/`glob` or raise `max_results`"
            ));
        }
        let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
        let preview = if hits_count == 0 {
            "no hits".to_string()
        } else {
            preview_body(&joined)
        };

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

struct Query {
    matcher: RegexMatcher,
    glob: Option<globset::GlobMatcher>,
    context_lines: usize,
    max_results: usize,
}

/// Blocking search of one file or a whole (cached) tree; returns the output
/// rows and the number of hits.
fn search(
    query: &Query,
    search_root: PathBuf,
    cwd: &Path,
    cancel: &CancellationToken,
) -> Result<(Vec<String>, usize), ToolError> {
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0x00))
        .before_context(query.context_lines)
        .after_context(query.context_lines)
        .build();
    let mut results = Vec::new();
    let mut total_hits = 0usize;
    let display = |path: &Path| {
        path.strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };

    if search_root.is_file() {
        let mut sink = CollectSink {
            matcher: &query.matcher,
            results: &mut results,
            display_path: &display(&search_root),
            total_hits: &mut total_hits,
            max_results: query.max_results,
        };
        let _ = searcher.search_path(&query.matcher, &search_root, &mut sink);
        return Ok((results, total_hits));
    }

    let entries = crate::walk::shared().collect(
        &search_root,
        crate::walk::WalkOptions {
            include_hidden: false,
            max_depth: None,
        },
        cancel,
    )?;

    for entry in entries.iter() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if total_hits >= query.max_results {
            break;
        }
        if !entry.is_file {
            continue;
        }
        if let Some(gm) = &query.glob {
            let filename = entry.rel.file_name().unwrap_or_default();
            if !gm.is_match(filename) && !gm.is_match(&entry.rel) {
                continue;
            }
        }
        // Join onto the non-canonical root so the display path strips
        // against `cwd` exactly like a fresh walk would.
        let path = search_root.join(&entry.rel);
        let mut sink = CollectSink {
            matcher: &query.matcher,
            results: &mut results,
            display_path: &display(&path),
            total_hits: &mut total_hits,
            max_results: query.max_results,
        };
        let _ = searcher.search_path(&query.matcher, &path, &mut sink);
    }

    Ok((results, total_hits))
}

struct CollectSink<'a> {
    matcher: &'a RegexMatcher,
    results: &'a mut Vec<String>,
    display_path: &'a str,
    total_hits: &'a mut usize,
    max_results: usize,
}

impl CollectSink<'_> {
    /// `path:N:line` for a hit, `path-N-line` for context.
    fn push(&mut self, sep: char, line_num: u64, bytes: &[u8]) {
        let line = String::from_utf8_lossy(bytes);
        let line = line.trim_end_matches(['\r', '\n']);
        let row = clip_line(line, self.matcher);
        self.results
            .push(format!("{}{sep}{line_num}{sep}{row}", self.display_path));
    }
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        *self.total_hits += 1;
        self.push(':', mat.line_number().unwrap_or(0), mat.bytes());
        Ok(*self.total_hits < self.max_results)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        self.push('-', ctx.line_number().unwrap_or(0), ctx.bytes());
        Ok(true)
    }
}

/// `line` unchanged when short; otherwise a `MAX_LINE_BYTES` window starting
/// a quarter-window before the first match (a context row has none and keeps
/// its start), `…` on each cut side, and the line's full length.
fn clip_line<'a>(line: &'a str, matcher: &RegexMatcher) -> Cow<'a, str> {
    use grep_matcher::Matcher as _;

    if line.len() <= MAX_LINE_BYTES {
        return Cow::Borrowed(line);
    }
    let hit_start = matcher
        .find(line.as_bytes())
        .ok()
        .flatten()
        .map_or(0, |m| m.start());
    let begin = line.floor_char_boundary(hit_start.saturating_sub(MAX_LINE_BYTES / 4));
    let end = line.floor_char_boundary(begin + MAX_LINE_BYTES);
    let lead = if begin > 0 { "…" } else { "" };
    let trail = if end < line.len() { "…" } else { "" };
    Cow::Owned(format!(
        "{lead}{}{trail} [line is {} bytes]",
        &line[begin..end],
        line.len()
    ))
}

#[cfg(test)]
#[path = "grep_tests.rs"]
mod grep_tests;
