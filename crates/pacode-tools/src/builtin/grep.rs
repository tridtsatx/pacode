//! `grep`: regex search with ripgrep's crates (`ignore` + `grep-searcher`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Candidate files per search thread; below this a thread costs more than
/// it saves, so small trees stay on one thread.
const FILES_PER_THREAD: usize = 64;
/// Ceiling on search threads per call: several agents may grep at once, and
/// the blocking pool already runs one thread per call.
const MAX_SEARCH_THREADS: usize = 8;
/// Line-buffer ceiling for a parallel search thread (the searcher's buffer
/// grows to the longest line it meets and keeps that size).
const WORKER_HEAP_LIMIT: usize = 1024 * 1024;

/// Blocking search of one file or a whole (cached) tree; returns the output
/// rows and the number of hits.
///
/// A tree is searched by up to `MAX_SEARCH_THREADS` threads pulling files in
/// walk order from a shared index. Threads stop taking files once
/// `max_results` hits are found; since files are handed out in order, every
/// file before the last one taken has been searched, so merging per-file
/// results in walk order yields exactly the rows a one-thread search would.
fn search(
    query: &Query,
    search_root: PathBuf,
    cwd: &Path,
    cancel: &CancellationToken,
) -> Result<(Vec<String>, usize), ToolError> {
    let display = |path: &Path| {
        path.strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };

    if search_root.is_file() {
        let file = match search_file(
            query,
            &mut new_searcher(query, None),
            &search_root,
            &display(&search_root),
        ) {
            Ok(hits) | Err(hits) => hits,
        };
        return Ok(merge([file], query.max_results));
    }

    let entries = crate::walk::shared().collect(
        &search_root,
        crate::walk::WalkOptions {
            include_hidden: false,
            max_depth: None,
        },
        cancel,
    )?;
    let files: Vec<&crate::walk::WalkEntry> = entries
        .iter()
        .filter(|entry| entry.is_file)
        .filter(|entry| {
            query.glob.as_ref().is_none_or(|gm| {
                gm.is_match(entry.rel.file_name().unwrap_or_default()) || gm.is_match(&entry.rel)
            })
        })
        .collect();

    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_SEARCH_THREADS);
    let workers = (files.len() / FILES_PER_THREAD).clamp(1, threads);
    let next = AtomicUsize::new(0);
    let found = AtomicUsize::new(0);
    // Each worker's line buffer is capped; a file with a longer line (a
    // minified bundle, a binary without an early NUL) fails there and is
    // searched again below by one uncapped searcher, so N threads never hold
    // N giant buffers at once and no file is skipped.
    let worker = || {
        let mut searcher = new_searcher(query, Some(WORKER_HEAP_LIMIT));
        let mut out = Vec::new();
        let mut retry = Vec::new();
        while !cancel.is_cancelled() && found.load(Ordering::Relaxed) < query.max_results {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(entry) = files.get(index) else { break };
            // Join onto the non-canonical root so the display path strips
            // against `cwd` exactly like a fresh walk would.
            let path = search_root.join(&entry.rel);
            match search_file(query, &mut searcher, &path, &display(&path)) {
                Ok(hits) if hits.hits > 0 => {
                    found.fetch_add(hits.hits, Ordering::Relaxed);
                    out.push((index, hits));
                }
                Ok(_) => {}
                Err(_) => retry.push(index),
            }
        }
        (out, retry)
    };
    let (mut per_file, retry) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers).map(|_| scope.spawn(worker)).collect();
        let mut all = Vec::new();
        let mut retry = Vec::new();
        for handle in handles {
            let (part, failed) = handle
                .join()
                .map_err(|_| ToolError::failed("grep search thread panicked"))?;
            all.extend(part);
            retry.extend(failed);
        }
        Ok::<_, ToolError>((all, retry))
    })?;
    let mut uncapped = new_searcher(query, None);
    for index in retry {
        if cancel.is_cancelled() {
            break;
        }
        let path = search_root.join(&files[index].rel);
        // A second failure is an IO error (permissions, a vanished file):
        // keep whatever rows came before it, as a single search would.
        let hits = match search_file(query, &mut uncapped, &path, &display(&path)) {
            Ok(hits) | Err(hits) => hits,
        };
        if hits.hits > 0 {
            per_file.push((index, hits));
        }
    }
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    per_file.sort_unstable_by_key(|(index, _)| *index);
    Ok(merge(
        per_file.into_iter().map(|(_, file)| file),
        query.max_results,
    ))
}

fn new_searcher(query: &Query, heap_limit: Option<usize>) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0x00))
        .before_context(query.context_lines)
        .after_context(query.context_lines)
        .heap_limit(heap_limit)
        .build()
}

/// One file's rows; the flag marks hit rows (vs context) so [`merge`] can cut
/// right after the `max_results`-th hit.
#[derive(Default)]
struct FileHits {
    rows: Vec<(bool, String)>,
    hits: usize,
}

/// `Err` carries the rows found before the search failed.
fn search_file(
    query: &Query,
    searcher: &mut Searcher,
    path: &Path,
    display: &str,
) -> Result<FileHits, FileHits> {
    let mut sink = CollectSink {
        matcher: &query.matcher,
        display_path: display,
        max_results: query.max_results,
        out: FileHits::default(),
    };
    match searcher.search_path(&query.matcher, path, &mut sink) {
        Ok(()) => Ok(sink.out),
        Err(_) => Err(sink.out),
    }
}

/// Rows of `files` in order, stopping right after the `max_results`-th hit —
/// where a single searcher would have stopped (no trailing context).
fn merge(files: impl IntoIterator<Item = FileHits>, max_results: usize) -> (Vec<String>, usize) {
    let mut rows = Vec::new();
    let mut hits = 0usize;
    'files: for file in files {
        for (is_hit, row) in file.rows {
            rows.push(row);
            if is_hit {
                hits += 1;
                if hits >= max_results {
                    break 'files;
                }
            }
        }
    }
    (rows, hits)
}

struct CollectSink<'a> {
    matcher: &'a RegexMatcher,
    display_path: &'a str,
    max_results: usize,
    out: FileHits,
}

impl CollectSink<'_> {
    /// `path:N:line` for a hit, `path-N-line` for context.
    fn push(&mut self, is_hit: bool, line_num: u64, bytes: &[u8]) {
        let sep = if is_hit { ':' } else { '-' };
        let line = String::from_utf8_lossy(bytes);
        let line = line.trim_end_matches(['\r', '\n']);
        let row = clip_line(line, self.matcher);
        self.out.rows.push((
            is_hit,
            format!("{}{sep}{line_num}{sep}{row}", self.display_path),
        ));
    }
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        self.out.hits += 1;
        self.push(true, mat.line_number().unwrap_or(0), mat.bytes());
        Ok(self.out.hits < self.max_results)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        self.push(false, ctx.line_number().unwrap_or(0), ctx.bytes());
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
    let begin = floor_char_boundary(line, hit_start.saturating_sub(MAX_LINE_BYTES / 4));
    let end = floor_char_boundary(line, begin + MAX_LINE_BYTES);
    let lead = if begin > 0 { "…" } else { "" };
    let trail = if end < line.len() { "…" } else { "" };
    Cow::Owned(format!(
        "{lead}{}{trail} [line is {} bytes]",
        &line[begin..end],
        line.len()
    ))
}

/// Largest char boundary `<= index` (`line.len()` past the end). Same as
/// `str::floor_char_boundary`, which is newer than the workspace MSRV (1.90).
fn floor_char_boundary(line: &str, index: usize) -> usize {
    if index >= line.len() {
        return line.len();
    }
    let mut index = index;
    while !line.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
#[path = "grep_tests.rs"]
mod grep_tests;
