//! `symbols`: tree-sitter outline of a file or directory — functions, methods,
//! classes, impls — with 1-based line ranges, plus a `context` mode that adds
//! the first/last lines of each body.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pacode_symbols::{FileSymbols, Language, Symbol, SymbolKind};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};
use symbols_parse::ParseAhead;

pub const NAME: &str = "symbols";

/// Model-facing description; see `prompts/symbols.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/symbols.md");

#[derive(Default)]
pub struct SymbolsTool {
    definition: DefinitionCache,
}

const DEFAULT_MAX_FILES: usize = 100;
const MAX_FILES_CAP: usize = 500;
const DEFAULT_MAX_SYMBOLS: usize = 400;
const MAX_SYMBOLS_CAP: usize = 2000;
const DEFAULT_HEAD_LINES: usize = 3;
const DEFAULT_TAIL_LINES: usize = 1;
const MAX_CONTEXT_LINES: usize = 20;
/// Context lines longer than this are abbreviated with `…`.
const MAX_LINE_CHARS: usize = 160;
/// When more files than this are skipped, only the aggregate counts are shown.
const SKIP_DETAIL_CAP: usize = 10;

/// File names that are always generated artifacts — a symbol map of them is
/// noise. Checked on the file name during directory traversal (an explicitly
/// named single file is processed anyway: the user asked for it).
const GENERATED_SUFFIXES: &[&str] = &[".min.js", ".pb.go", ".g.dart", ".designer.cs"];

fn is_generated(name: &str) -> bool {
    GENERATED_SUFFIXES.iter().any(|s| name.ends_with(s)) || name.contains(".generated.")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SymbolsInput {
    path: String,
    glob: Option<String>,
    mode: Option<String>,
    lang: Option<String>,
    kinds: Option<Vec<String>>,
    depth: Option<usize>,
    head_lines: Option<usize>,
    tail_lines: Option<usize>,
    max_files: Option<usize>,
    max_symbols: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Outline,
    Context,
}

/// A candidate file's place in the scan, decided before any file is read.
enum Step {
    Skip(String, SkipReason),
    /// Display path and grammar of a file queued for parsing.
    Parse(String, Language),
}

/// Why a candidate file produced no section — always reported, never silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SkipReason {
    Binary,
    NotUtf8,
    TooLarge,
    NoGrammar,
    Generated,
    Unreadable,
    ParseFailed,
}

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            SkipReason::Binary => "binary",
            SkipReason::NotUtf8 => "not utf-8",
            SkipReason::TooLarge => "too large",
            SkipReason::NoGrammar => "no grammar",
            SkipReason::Generated => "generated",
            SkipReason::Unreadable => "unreadable",
            SkipReason::ParseFailed => "parse failed",
        }
    }
}

#[derive(Clone)]
struct Options {
    mode: Mode,
    kinds: Vec<SymbolKind>,
    /// Max nesting depth shown; `usize::MAX` = unlimited.
    depth: usize,
    head_lines: usize,
    tail_lines: usize,
    max_files: usize,
    max_symbols: usize,
}

struct FileReport {
    /// Display path (cwd-relative when possible).
    display: String,
    /// File text — kept only in `context` mode (needed for body excerpts);
    /// empty in `outline` mode so a 500-file scan doesn't retain megabytes.
    source: String,
    symbols: FileSymbols,
    lang: Language,
}

struct Scan {
    reports: Vec<FileReport>,
    skipped: Vec<(String, SkipReason)>,
    /// Files that went through the read+parse pipeline — the `max_files`
    /// budget unit (cheap skips like `no grammar` don't consume it).
    attempted: usize,
    /// Candidates evaluated — includes every skipped file.
    visited: usize,
    /// Set when `max_files` stopped the scan before all candidates.
    files_truncated: bool,
    /// Set when `max_symbols` stopped symbol emission mid-scan.
    symbols_truncated: bool,
    /// Symbol rows the parsed reports would emit — feeds the early-stop check.
    symbols_shown: usize,
}

#[async_trait]
impl Tool for SymbolsTool {
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
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "description": "File or directory, absolute or relative to the working directory."},
                "glob": {"type": "string", "description": "Limit directory scans to files matching this pattern (e.g. `**/*.rs`). Ignored for a single file."},
                "mode": {"type": "string", "enum": ["outline", "context"], "default": "outline", "description": "`context` adds the first/last lines of each symbol body."},
                "lang": {"type": "string", "description": "Language override for a single file (e.g. `rust`, `py`). Invalid for directories."},
                "kinds": {"type": "array", "items": {"type": "string"}, "description": "Keep only these kinds: fn, method, class, struct, enum, trait, impl, interface, const, type, module, export, macro."},
                "depth": {"type": "integer", "minimum": 0, "description": "Maximum nesting level shown (0 = top-level only; default unlimited)."},
                "head_lines": {"type": "integer", "minimum": 0, "description": "Context mode: leading body lines per symbol (default 3, max 20)."},
                "tail_lines": {"type": "integer", "minimum": 0, "description": "Context mode: trailing body lines per symbol (default 1, max 20)."},
                "max_files": {"type": "integer", "minimum": 1, "description": "Cap on parsed files (default 100, max 500)."},
                "max_symbols": {"type": "integer", "minimum": 1, "description": "Cap on emitted symbol rows (default 400, max 2000)."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<SymbolsInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }

        let mode = match args.mode.as_deref() {
            None | Some("outline") => Mode::Outline,
            Some("context") => Mode::Context,
            Some(other) => {
                return Err(ToolError::invalid(format!(
                    "unknown mode `{other}` (expected `outline` or `context`)"
                )));
            }
        };

        let mut kinds = Vec::new();
        for raw in args.kinds.iter().flatten() {
            let Some(kind) = SymbolKind::from_name(raw) else {
                let valid: Vec<&str> = SymbolKind::ALL.iter().map(|k| k.name()).collect();
                return Err(ToolError::invalid(format!(
                    "unknown kind `{raw}` (expected one of: {})",
                    valid.join(", ")
                )));
            };
            kinds.push(kind);
        }
        kinds.sort_unstable();
        kinds.dedup();

        let lang_override = args
            .lang
            .as_deref()
            .map(|name| {
                Language::from_name(name).ok_or_else(|| {
                    let valid: Vec<&str> = Language::ALL.iter().map(|l| l.name()).collect();
                    ToolError::invalid(format!(
                        "unknown lang `{name}` (expected one of: {})",
                        valid.join(", ")
                    ))
                })
            })
            .transpose()?;

        let opts = Options {
            mode,
            kinds,
            depth: args.depth.unwrap_or(usize::MAX),
            head_lines: args
                .head_lines
                .unwrap_or(DEFAULT_HEAD_LINES)
                .min(MAX_CONTEXT_LINES),
            tail_lines: args
                .tail_lines
                .unwrap_or(DEFAULT_TAIL_LINES)
                .min(MAX_CONTEXT_LINES),
            max_files: args
                .max_files
                .unwrap_or(DEFAULT_MAX_FILES)
                .clamp(1, MAX_FILES_CAP),
            max_symbols: args
                .max_symbols
                .unwrap_or(DEFAULT_MAX_SYMBOLS)
                .clamp(1, MAX_SYMBOLS_CAP),
        };

        let full_path = ctx.resolve(Path::new(&args.path));
        let cwd = ctx.cwd.clone();
        let cancel = ctx.cancel.clone();
        let glob_pattern = args.glob.clone();
        let arg_path = args.path.clone();
        let scan_opts = opts.clone();

        let scan = tokio::task::spawn_blocking(move || {
            scan_path(
                &full_path,
                &cwd,
                &arg_path,
                glob_pattern,
                lang_override,
                &scan_opts,
                &cancel,
            )
        })
        .await
        .map_err(|e| ToolError::failed(e.to_string()))??;

        let content = if scan.reports.is_empty() && scan.skipped.is_empty() {
            format!("no files matched under {}", args.path)
        } else {
            render(&scan, &opts)
        };
        let content = cap_output(&content, accept_large_output, ctx.output_cap_chars);
        let preview = preview_body(&content);
        let title = format!("Symbols {}", args.path);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

fn display_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |rel| rel.to_string_lossy().into_owned(),
    )
}

/// Blocking filesystem walk + parse. Returns a [`Scan`] with per-file symbol
/// reports, a `skipped` ledger, and the summary counters.
fn scan_path(
    full_path: &Path,
    cwd: &Path,
    arg_path: &str,
    glob_pattern: Option<String>,
    lang_override: Option<Language>,
    opts: &Options,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Scan, ToolError> {
    let meta = std::fs::metadata(full_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::invalid(format!("path does not exist: {arg_path}"))
        } else {
            ToolError::Io(e)
        }
    })?;

    let mut scan = Scan {
        reports: Vec::new(),
        skipped: Vec::new(),
        attempted: 0,
        visited: 0,
        files_truncated: false,
        symbols_truncated: false,
        symbols_shown: 0,
    };

    // (absolute path, display path) for each candidate, lexicographically
    // sorted so output is deterministic.
    let files: Vec<(PathBuf, String)> = if meta.is_file() {
        vec![(full_path.to_path_buf(), arg_path.to_string())]
    } else if meta.is_dir() {
        if lang_override.is_some() {
            return Err(ToolError::invalid(
                "`lang` can only override a single file, not a directory",
            ));
        }
        let matcher = glob_pattern
            .as_deref()
            .map(|pattern| {
                globset::GlobBuilder::new(pattern)
                    .literal_separator(false)
                    .build()
                    .map_err(|e| ToolError::invalid(format!("invalid glob pattern: {e}")))
                    .map(|g| g.compile_matcher())
            })
            .transpose()?;

        let entries = crate::walk::shared().collect(
            full_path,
            crate::walk::WalkOptions {
                include_hidden: false,
                max_depth: None,
            },
            cancel,
        )?;

        let mut matched: Vec<PathBuf> = entries
            .iter()
            .filter(|e| e.is_file)
            .filter(|e| {
                matcher.as_ref().is_none_or(|m| {
                    m.is_match(&e.rel) || m.is_match(e.rel.file_name().unwrap_or_default())
                })
            })
            .map(|e| full_path.join(&e.rel))
            .collect();
        matched.sort();
        matched
            .into_iter()
            .map(|p| (p.clone(), display_path(&p, cwd)))
            .collect()
    } else {
        return Err(ToolError::invalid(format!(
            "path is neither a file nor a directory: {arg_path}"
        )));
    };

    // The cheap verdicts (generated, no grammar) need no read; the files left
    // are queued for parsing, which runs ahead of the budget checks below.
    let mut plan = Vec::with_capacity(files.len());
    let mut queue = Vec::new();
    for (path, display) in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if lang_override.is_none() && meta.is_dir() && is_generated(name) {
            plan.push(Step::Skip(display, SkipReason::Generated));
            continue;
        }
        match lang_override.or_else(|| Language::from_path(&path)) {
            Some(lang) => {
                queue.push((path, lang));
                plan.push(Step::Parse(display, lang));
            }
            None => plan.push(Step::Skip(display, SkipReason::NoGrammar)),
        }
    }
    let mut parsed = ParseAhead::new(queue, opts.mode == Mode::Context);

    for step in plan {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        scan.visited += 1;

        let (display, lang) = match step {
            Step::Skip(display, reason) => {
                scan.skipped.push((display, reason));
                continue;
            }
            Step::Parse(display, lang) => (display, lang),
        };

        if scan.attempted >= opts.max_files {
            scan.files_truncated = true;
            break;
        }
        if scan.symbols_shown >= opts.max_symbols {
            scan.symbols_truncated = true;
            break;
        }

        scan.attempted += 1;
        // `source` is empty unless `context` mode asked `ParseAhead` to keep it.
        match parsed.next(opts.max_files - scan.attempted + 1, cancel)? {
            Ok((source, symbols)) => {
                scan.symbols_shown += count_shown(&symbols.symbols, opts);
                scan.reports.push(FileReport {
                    display,
                    source,
                    symbols,
                    lang,
                });
            }
            Err(reason) => scan.skipped.push((display, reason)),
        }
    }

    Ok(scan)
}

/// Count symbol rows a file would emit under `opts` — needed before pushing
/// the report so the `max_symbols` check stops the scan early.
fn count_shown(symbols: &[Symbol], opts: &Options) -> usize {
    symbols
        .iter()
        .filter(|s| {
            (opts.kinds.is_empty() || opts.kinds.contains(&s.kind))
                && s.depth(symbols) <= opts.depth
        })
        .count()
}

fn render(scan: &Scan, opts: &Options) -> String {
    let mut out = String::new();
    let mut shown = 0usize;

    for report in &scan.reports {
        let _ = write!(out, "{} ({})", report.display, report.lang.name());
        if report.symbols.parsed_with_errors {
            match report.symbols.first_error_line {
                Some(line) => {
                    let _ = write!(out, " — parsed with errors (first at line {line})");
                }
                None => {
                    let _ = write!(out, " — parsed with errors");
                }
            }
        }
        out.push('\n');

        if shown >= opts.max_symbols {
            continue;
        }
        let lines: Vec<&str> = report.source.lines().collect();
        for sym in &report.symbols.symbols {
            if !opts.kinds.is_empty() && !opts.kinds.contains(&sym.kind) {
                continue;
            }
            let depth = sym.depth(&report.symbols.symbols);
            if depth > opts.depth {
                continue;
            }
            if shown >= opts.max_symbols {
                break;
            }
            shown += 1;
            let indent = "  ".repeat(depth + 1);
            let range = format!("{}-{}", sym.start_line, sym.end_line);
            let _ = writeln!(
                out,
                "{indent}{range:<10}{kind:<8}{name}",
                kind = sym.kind.name(),
                name = sym.name
            );
            if opts.mode == Mode::Context {
                render_context(&mut out, &lines, sym, depth, opts);
            }
        }
    }

    render_skipped(&mut out, scan);
    // Symbol emission may have stopped mid-file even when the scan itself
    // finished (last parsed file overflowed the budget exactly).
    let would_emit: usize = scan
        .reports
        .iter()
        .map(|r| count_shown(&r.symbols.symbols, opts))
        .sum();
    let render_truncated = would_emit > shown;
    render_footer(&mut out, scan, shown, render_truncated);
    out
}

fn render_context(out: &mut String, lines: &[&str], sym: &Symbol, depth: usize, opts: &Options) {
    let indent = "  ".repeat(depth + 2);
    let start = sym.start_line as usize;
    let end = sym.end_line as usize;
    if start == 0 || end < start || end > lines.len() {
        return;
    }
    let body: Vec<(usize, &str)> = (start..=end).map(|n| (n, lines[n - 1])).collect();
    let whole = body.len() <= opts.head_lines + opts.tail_lines + 2;
    let emit = |out: &mut String, (n, text): (usize, &str)| {
        let _ = writeln!(out, "{indent}{n:>6}\t{}", abbreviate(text));
    };
    if whole {
        for row in body {
            emit(out, row);
        }
    } else {
        for row in body.iter().take(opts.head_lines) {
            emit(out, *row);
        }
        let _ = writeln!(out, "{indent}     …");
        for row in body.iter().skip(body.len() - opts.tail_lines) {
            emit(out, *row);
        }
    }
}

fn abbreviate(text: &str) -> String {
    if text.chars().count() <= MAX_LINE_CHARS {
        text.to_string()
    } else {
        let head: String = text.chars().take(MAX_LINE_CHARS).collect();
        format!("{head}…")
    }
}

fn render_skipped(out: &mut String, scan: &Scan) {
    if scan.skipped.is_empty() {
        return;
    }
    let mut counts = std::collections::BTreeMap::new();
    for (_, reason) in &scan.skipped {
        *counts.entry(*reason).or_insert(0usize) += 1;
    }
    let breakdown = counts
        .iter()
        .map(|(reason, n)| format!("{n} {}", reason.label()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "\nskipped: {} ({breakdown})", scan.skipped.len());
    if scan.skipped.len() <= SKIP_DETAIL_CAP {
        for (display, reason) in &scan.skipped {
            let _ = writeln!(out, "  {display} ({})", reason.label());
        }
    }
}

fn render_footer(out: &mut String, scan: &Scan, shown: usize, render_truncated: bool) {
    let mut truncated = Vec::new();
    if scan.symbols_truncated || render_truncated {
        truncated.push("max_symbols hit (narrow with path/glob/kinds)");
    }
    if scan.files_truncated {
        truncated.push("max_files hit");
    }
    let files = plural(scan.visited, "file", "files");
    let _ = write!(
        out,
        "\n— {files} visited, {} parsed, {shown} symbols shown",
        scan.reports.len()
    );
    if !truncated.is_empty() {
        let _ = write!(out, "; truncated: {}", truncated.join(", "));
    }
    out.push('\n');
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

#[path = "symbols_parse.rs"]
mod symbols_parse;

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod symbols_tests;
