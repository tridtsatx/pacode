//! Smart `read`: big files come back as an outline (`pacode_outline`: signatures and
//! the start of every body kept, the rest behind `… N lines hidden (La-Lb) …` markers),
//! and hidden code is shown on request by symbol (`symbol:"Foo::bar"`) or by line
//! range (`expand:["44-80"]`). `read.rs` calls [`smart_read`] before its plain path.
//!
//! Only the lines actually shown carry anchors and are reported to the file ledger as
//! seen, so editing hidden code needs a `symbol`/`expand` read first.
//! No state: each call parses afresh (bounded by `pacode_outline`'s caps) on the
//! blocking pool.

use std::fmt::Write as _;
use std::ops::Range;
use std::path::Path;

use async_trait::async_trait;
use pacode_outline::{Hidden, Lang, Outline, OutlineOpts, Segment, find_symbol, symbol_paths};
use pacode_types::ReadConfig;
use serde::Deserialize;
use serde_json::Value;

use super::helpers::cap_output;
use super::read::ReadTool;
use crate::hashline::annotated_line;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

/// Auto mode shows an outline only when it is below this share of the plain read.
const WORTH_PERCENT: usize = 60;
/// Symbol matches rendered in full; the rest are listed by name.
const MAX_SYMBOL_HITS: usize = 10;
/// Symbol names suggested when a lookup finds nothing.
const MAX_SUGGESTIONS: usize = 40;
/// Marker indentation is copied from the first hidden line, up to this many bytes.
const MAX_MARKER_INDENT: usize = 32;

/// The `mode` parameter of `read`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    /// Outline when the file is over `[read].outline_threshold` lines, its language is
    /// supported and the outline is clearly shorter; the plain file otherwise.
    #[default]
    Auto,
    /// Always the plain file.
    Full,
    /// Always an outline (an error when the file cannot be outlined).
    Outline,
}

/// The smart-read part of a `read` call.
pub(crate) struct SmartArgs<'a> {
    pub mode: ReadMode,
    pub symbol: Option<&'a str>,
    pub expand: &'a [String],
    /// `offset` or `limit` was given: always a plain ranged read.
    pub ranged: bool,
}

/// What a smart read produced, before the output cap.
struct Rendered {
    text: String,
    shown: Vec<Range<usize>>,
    preview: String,
}

/// `Some(output)` when the call is answered here (outline, symbol, expand); `None`
/// sends it down the plain path of `read`. `full_path` is the resolved file,
/// `shown_path` the path as the model wrote it.
pub(crate) async fn smart_read(
    args: &SmartArgs<'_>,
    text: &str,
    full_path: &Path,
    shown_path: &str,
    settings: &ReadConfig,
    ctx: &ToolCtx,
    accept_large_output: bool,
) -> Result<Option<ToolOutput>, ToolError> {
    let targeted = args.symbol.is_some() || !args.expand.is_empty();
    if args.ranged {
        if targeted {
            return Err(ToolError::invalid(
                "offset/limit cannot be combined with symbol/expand",
            ));
        }
        return Ok(None);
    }
    let lines: Vec<&str> = text.lines().collect();
    let rendered = if targeted {
        targeted_read(args, text, &lines, full_path, shown_path).await?
    } else {
        match args.mode {
            ReadMode::Full => return Ok(None),
            ReadMode::Auto => {
                let threshold = settings.outline_threshold;
                if threshold == 0 || lines.len() <= threshold {
                    return Ok(None);
                }
                let Ok(outline) = outline_of(text, full_path).await else {
                    return Ok(None);
                };
                let rendered = render_outline(&outline, &lines, shown_path);
                if rendered.text.len() * 100 >= plain_len(&lines) * WORTH_PERCENT {
                    return Ok(None);
                }
                rendered
            }
            ReadMode::Outline => {
                let outline = outline_of(text, full_path).await.map_err(|e| {
                    ToolError::failed(format!("{e}; use mode:\"full\" or offset/limit"))
                })?;
                render_outline(&outline, &lines, shown_path)
            }
        }
    };
    // Only shown rows count as seen. One ledger call per range (the ledger unions
    // them); each call re-hashes the file, which is fine at outline sizes (<= 2 MiB).
    for range in &rendered.shown {
        ctx.host
            .ledger_record_read(&ctx.agent, full_path, text, range.clone())
            .await;
    }
    let content = cap_output(&rendered.text, accept_large_output, ctx.output_cap_chars);
    Ok(Some(
        ToolOutput::text(content)
            .with_title(format!("Read {shown_path}"))
            .with_preview(rendered.preview),
    ))
}

/// Outline `text` on the blocking pool (parsing is CPU-bound, up to the parse timeout).
async fn outline_of(text: &str, path: &Path) -> Result<Outline, String> {
    let lang = Lang::from_path(path).ok_or_else(|| "no outline for this file type".to_string())?;
    let opts = OutlineOpts::default();
    if text.len() > opts.max_input_bytes {
        return Err(format!(
            "file too large for an outline ({} bytes, cap {})",
            text.len(),
            opts.max_input_bytes
        ));
    }
    let owned = text.to_string();
    tokio::task::spawn_blocking(move || pacode_outline::outline(&owned, lang, &opts))
        .await
        .map_err(|e| format!("outline task failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// `symbol` and/or `expand`: the requested code in full, with anchors.
async fn targeted_read(
    args: &SmartArgs<'_>,
    text: &str,
    lines: &[&str],
    full_path: &Path,
    shown_path: &str,
) -> Result<Rendered, ToolError> {
    let mut blocks: Vec<(String, Range<usize>)> = Vec::new();
    let mut extra = String::new();
    if let Some(query) = args.symbol {
        let outline = outline_of(text, full_path).await.map_err(|e| {
            ToolError::failed(format!(
                "cannot look up symbols: {e}; use expand:[\"a-b\"] or offset/limit"
            ))
        })?;
        let hits = find_symbol(&outline, query);
        if hits.is_empty() {
            return Err(ToolError::failed(no_symbol_message(
                query, shown_path, &outline,
            )));
        }
        for hit in hits.iter().take(MAX_SYMBOL_HITS) {
            let header = format!("{} {} in {shown_path}", hit.kind, hit.path);
            blocks.push((header, hit.lines.clone()));
        }
        if hits.len() > MAX_SYMBOL_HITS {
            let rest: Vec<String> = hits
                .iter()
                .skip(MAX_SYMBOL_HITS)
                .map(|h| format!("{} (L{}-L{})", h.path, h.lines.start + 1, h.lines.end))
                .collect();
            let _ = write!(
                extra,
                "\n\n{} more matches: {}",
                rest.len(),
                rest.join(", ")
            );
        }
    }
    for spec in args.expand {
        let range = parse_range(spec, lines.len())?;
        blocks.push((shown_path.to_string(), range));
    }
    let mut out = String::new();
    let mut shown = Vec::new();
    for (k, (header, range)) in blocks.iter().enumerate() {
        if k > 0 {
            out.push_str("\n\n");
        }
        let _ = writeln!(out, "{header} (L{}-L{}):", range.start + 1, range.end);
        out.push_str(&anchored(lines, range.clone()));
        shown.push(range.clone());
    }
    out.push_str(&extra);
    let preview = match (args.symbol, blocks.len()) {
        (Some(q), _) => format!("symbol {q}"),
        (None, 1) => "1 range".to_string(),
        (None, n) => format!("{n} ranges"),
    };
    Ok(Rendered {
        text: out,
        shown,
        preview,
    })
}

fn no_symbol_message(query: &str, shown_path: &str, outline: &Outline) -> String {
    let paths = symbol_paths(outline);
    if paths.is_empty() {
        return format!("no symbol \"{query}\" in {shown_path} (no declarations found)");
    }
    let listed: Vec<&str> = paths
        .iter()
        .take(MAX_SUGGESTIONS)
        .map(String::as_str)
        .collect();
    let more = paths.len().saturating_sub(MAX_SUGGESTIONS);
    let tail = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    format!(
        "no symbol \"{query}\" in {shown_path}; symbols: {}{tail}",
        listed.join(", ")
    )
}

/// `"44-80"`, `"L44-L80"` or `"44"` (1-based, inclusive) → 0-based rows. The end is
/// clamped to the file; a start past the end is an error.
pub(crate) fn parse_range(spec: &str, total: usize) -> Result<Range<usize>, ToolError> {
    let bad = || {
        ToolError::invalid(format!(
            "expand: bad range \"{spec}\" (use \"a-b\", 1-based)"
        ))
    };
    let number = |s: &str| -> Result<usize, ToolError> {
        let s = s.trim();
        let s = s.strip_prefix(['L', 'l']).unwrap_or(s);
        s.parse::<usize>().map_err(|_| bad())
    };
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (number(a)?, number(b)?),
        None => {
            let a = number(spec)?;
            (a, a)
        }
    };
    if a == 0 || b < a {
        return Err(bad());
    }
    if a > total {
        return Err(ToolError::invalid(format!(
            "expand: range \"{spec}\" starts past the end of the file ({total} lines)"
        )));
    }
    Ok(a - 1..b.min(total))
}

/// Anchored rows `range` of `lines`, joined by `\n`.
fn anchored(lines: &[&str], range: Range<usize>) -> String {
    range
        .filter_map(|i| lines.get(i).map(|l| annotated_line(i + 1, l)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Header, anchored shown lines and one marker per hidden region.
fn render_outline(outline: &Outline, lines: &[&str], shown_path: &str) -> Rendered {
    let mut out = format!(
        "outline of {shown_path} ({} lines, {} shown); use symbol:\"name\" or \
         expand:[\"a-b\"] to see hidden code; mode:\"full\" for everything",
        outline.total_lines,
        outline.shown_lines()
    );
    for segment in outline.segments() {
        out.push('\n');
        match segment {
            Segment::Shown(range) => out.push_str(&anchored(lines, range.clone())),
            Segment::Hidden(hidden) => out.push_str(&marker(hidden, lines)),
        }
    }
    Rendered {
        text: out,
        shown: outline.kept.clone(),
        preview: format!(
            "outline: {} of {} lines",
            outline.shown_lines(),
            outline.total_lines
        ),
    }
}

/// `    … 37 lines hidden (L44-L80): fn foo …`, indented like the hidden code. Never
/// looks like an anchor, so it cannot be mistaken for an editable line.
pub(crate) fn marker(hidden: &Hidden, lines: &[&str]) -> String {
    let range = &hidden.lines;
    let indent: String = range
        .clone()
        .filter_map(|i| lines.get(i))
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .take(MAX_MARKER_INDENT)
                .collect()
        })
        .unwrap_or_default();
    let label = hidden
        .label
        .as_deref()
        .map(|l| format!(": {l}"))
        .unwrap_or_default();
    format!(
        "{indent}… {} lines hidden (L{}-L{}){label} …",
        range.len(),
        range.start + 1,
        range.end
    )
}

/// Length of the plain anchored read (`N#hh| text` per line), for the auto-mode check.
fn plain_len(lines: &[&str]) -> usize {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| (i + 1).to_string().len() + 5 + l.len() + 1)
        .sum()
}

/// `read` with the `[read]` settings of the config; the daemon registers it over the
/// default [`ReadTool`] (same name, description and schema).
pub struct ConfiguredRead {
    settings: ReadConfig,
}

impl ConfiguredRead {
    pub fn new(settings: ReadConfig) -> Self {
        Self { settings }
    }
}

static READ: ReadTool = ReadTool;

#[async_trait]
impl Tool for ConfiguredRead {
    fn name(&self) -> &str {
        READ.name()
    }

    fn description(&self) -> &str {
        READ.description()
    }

    fn schema(&self) -> Value {
        READ.schema()
    }

    fn kind(&self) -> ToolKind {
        READ.kind()
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        READ.call_with(input, ctx, &self.settings).await
    }
}

#[cfg(test)]
#[path = "read_outline_tests.rs"]
mod read_outline_tests;
