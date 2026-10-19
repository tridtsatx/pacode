//! `read`: file contents with hashline anchors (`12#ab| code`) for the `edit` tool.

use std::path::Path;

use async_trait::async_trait;
use pacode_types::ReadConfig;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use super::read_outline::{ReadMode, SmartArgs, smart_read};
use crate::hashline::annotate_lines;
use crate::host::PermissionDraft;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

const DEFAULT_LINE_LIMIT: usize = 2000;

/// Text files above this size are never loaded whole: only a window of lines is
/// streamed from disk (the first [`DEFAULT_LINE_LIMIT`] without `offset`/`limit`, else
/// the requested range). Binary files keep the whole-file path (image branch).
pub(crate) const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// How many of the first bytes decide "this is binary".
const BINARY_SNIFF_BYTES: usize = 8192;

/// A unit struct (also constructible with `default()`): `read_outline::ConfiguredRead`
/// keeps a `static` of it, so it carries no `DefinitionCache`; the registry's own
/// definitions cache covers the per-step cost.
#[derive(Default)]
pub struct ReadTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    /// `false` opts out of anchors and gives plain `{:>6}\t{line}` numbering.
    anchors: Option<bool>,
    /// `auto` (default) / `full` / `outline`, see `read_outline`.
    mode: Option<ReadMode>,
    /// Show this declaration in full (`bar`, `Foo::bar`).
    symbol: Option<String>,
    /// Show these 1-based line ranges in full (`"44-80"`).
    expand: Vec<String>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("../prompts/read.md").trim()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                "offset": {"type": "integer", "minimum": 1, "description": "First line to return (1-based)."},
                "limit": {"type": "integer", "minimum": 1, "description": "Max lines to return (default 2000)."},
                "anchors": {"type": "boolean", "default": true, "description": "Prefix lines with edit anchors (default true); false = plain line numbers."},
                "mode": {"type": "string", "enum": ["auto", "full", "outline"], "default": "auto", "description": "auto: big source files as an outline; full: always the whole file; outline: always an outline. offset/limit always read plainly."},
                "symbol": {"type": "string", "description": "Return this declaration in full with anchors, e.g. \"bar\" or \"Foo::bar\"."},
                "expand": {"type": "array", "items": {"type": "string"}, "description": "Return these 1-based line ranges in full with anchors, e.g. [\"44-80\"] (from outline markers)."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `call_with` the default `[read]` settings (the daemon registers
    /// `read_outline::ConfiguredRead` with the configured ones).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.call_with(input, ctx, &ReadConfig::default()).await
    }
}

impl ReadTool {
    /// Asks before pulling a secret file into the context, refuses anything but a
    /// regular file (a fifo would block on open), reads the file (tokio::fs), rejects
    /// files with a NUL byte in the first 8 KiB, answers outline / `symbol` / `expand`
    /// reads via `read_outline::smart_read`, else applies offset/limit, formats
    /// `LINE#HASH| line` (or `{:>6}\t{line}` with `anchors:false`), appends a
    /// `— N more lines; read with offset=` footer when the listing stopped short, caps
    /// to `ctx.output_cap_chars` (unless `accept_large_output`), title `Read <path>`,
    /// preview = the listing (bounded) so the transcript cell can show and expand it.
    pub(crate) async fn call_with(
        &self,
        input: Value,
        ctx: &ToolCtx,
        settings: &ReadConfig,
    ) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<ReadInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        // Reading a credential or secret file pulls it into the model's
        // context. `read` is ToolKind::ReadOnly (allowed in every mode without
        // a prompt), so route a secret read through the same predicate the
        // shell gate uses and ask with Exec/Confirm semantics (Build and Auto
        // ask, Plan denies). grep/glob/ls/symbols skip hidden files, so they do
        // not surface `.env`/`.ssh`; `read` with an explicit path is the one
        // tool that otherwise reads a secret unprompted. Asked before the ledger
        // lock below, so a pending prompt never holds up writers of the path.
        if pacode_risk::is_protected_read(&args.path)
            || pacode_risk::is_protected_read(&full_path.to_string_lossy())
        {
            let draft = PermissionDraft::new(
                ctx.agent.clone(),
                ctx.agent_name.clone(),
                ctx.call_id.clone(),
                format!("Read secret file: {}", args.path),
                format!(
                    "{} is a credential or secret file; reading it pulls it into the model's \
                     context.",
                    args.path
                ),
                Some(pacode_risk::RiskLevel::Confirm),
            )
            .with_tool(NAME, ToolKind::Exec);
            ctx.host
                .request_permission(draft)
                .await
                .into_result("user denied reading the secret file")?;
        }
        let meta = tokio::fs::metadata(&full_path).await?;
        if !meta.is_file() {
            return Err(ToolError::failed(format!(
                "cannot read {}: not a regular file",
                args.path
            )));
        }
        // Under the path's write lock, so the ledger records the version actually read.
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;
        let request = SmartArgs {
            mode: match args.anchors {
                Some(false) => ReadMode::Full,
                _ => args.mode.unwrap_or_default(),
            },
            symbol: args.symbol.as_deref(),
            expand: &args.expand,
            ranged: args.offset.is_some() || args.limit.is_some(),
        };
        let size = tokio::fs::metadata(&full_path).await?.len();
        if size > MAX_READ_BYTES && !has_nul_prefix(&full_path).await? {
            return read_large_text(
                &args,
                &request,
                &full_path,
                settings,
                ctx,
                accept_large_output,
            )
            .await;
        }
        let bytes = tokio::fs::read(&full_path).await?;
        if let Some(out) = super::read_image::image_output(&bytes, &args.path).await {
            return Ok(out);
        }
        let check_len = bytes.len().min(BINARY_SNIFF_BYTES);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed(format!(
                "cannot read binary file: {}",
                args.path
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;
        let answered = smart_read(
            &request,
            &text,
            &full_path,
            &args.path,
            settings,
            ctx,
            accept_large_output,
        )
        .await?;
        if let Some(out) = answered {
            return Ok(out);
        }

        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_LINE_LIMIT);

        let total_lines = text.lines().count();
        let selected: Vec<&str> = text.lines().skip(offset - 1).take(limit).collect();
        let shown = (offset - 1)..(offset - 1 + selected.len());
        ctx.host
            .ledger_record_read(&ctx.agent, &full_path, &text, shown)
            .await;
        // When the listing stopped short, a footer names the remainder and the
        // `offset` that continues it, so the model can page on its own. The char
        // cap needs no hint — `cap_output` already reports its own cut.
        let next_offset = offset + selected.len();
        let remaining = total_lines.saturating_sub(next_offset.saturating_sub(1));
        let footer = (remaining > 0).then(|| {
            let noun = if remaining == 1 { "line" } else { "lines" };
            format!("{remaining} more {noun}; read with offset={next_offset}")
        });
        Ok(render_lines(
            &selected,
            offset,
            args.anchors.unwrap_or(true),
            footer.as_deref(),
            &args.path,
            ctx,
            accept_large_output,
        ))
    }
}

/// Formats the selected lines (anchored or plain numbering) plus an optional `— note`
/// footer, capped, with title and preview.
fn render_lines(
    selected: &[&str],
    offset: usize,
    anchors: bool,
    footer: Option<&str>,
    shown_path: &str,
    ctx: &ToolCtx,
    accept_large_output: bool,
) -> ToolOutput {
    let mut raw = if anchors {
        annotate_lines(selected.iter().copied(), offset)
    } else {
        selected
            .iter()
            .enumerate()
            .map(|(k, line)| format!("{:>6}\t{line}", offset + k))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if let Some(note) = footer {
        raw.push_str(&format!("\n— {note}"));
    }
    let content = cap_output(&raw, accept_large_output, ctx.output_cap_chars);
    let preview = preview_body(&content);
    ToolOutput::text(content)
        .with_title(format!("Read {shown_path}"))
        .with_preview(preview)
}

/// Whether the first 8 KiB of the file contain a NUL byte (the binary test of the full path).
async fn has_nul_prefix(path: &Path) -> std::io::Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(BINARY_SNIFF_BYTES);
    tokio::fs::File::open(path)
        .await?
        .take(BINARY_SNIFF_BYTES as u64)
        .read_to_end(&mut head)
        .await?;
    Ok(head.contains(&0))
}

/// A text file above `MAX_READ_BYTES` is never loaded whole. With `offset`/`limit` only
/// that range is streamed from disk; without, the first [`DEFAULT_LINE_LIMIT`] lines are,
/// with a footer saying the file was not read whole and how to go on.
async fn read_large_text(
    args: &ReadInput,
    request: &SmartArgs<'_>,
    full_path: &Path,
    settings: &ReadConfig,
    ctx: &ToolCtx,
    accept_large_output: bool,
) -> Result<ToolOutput, ToolError> {
    if request.ranged {
        // A ranged request never reads `text` in smart_read; it only rejects symbol/expand.
        if let Some(out) = smart_read(
            request,
            "",
            full_path,
            &args.path,
            settings,
            ctx,
            accept_large_output,
        )
        .await?
        {
            return Ok(out);
        }
    } else if request.symbol.is_some() || !request.expand.is_empty() {
        return Err(ToolError::invalid(format!(
            "{} is larger than the {MAX_READ_BYTES} byte read cap, so `symbol`/`expand` \
             cannot outline it; read a range with offset/limit or search it with grep",
            args.path
        )));
    }
    let offset = args.offset.unwrap_or(1).max(1);
    let limit = args.limit.unwrap_or(DEFAULT_LINE_LIMIT);
    let path = full_path.to_path_buf();
    let selected = tokio::task::spawn_blocking(move || read_line_range(&path, offset, limit))
        .await
        .map_err(|e| ToolError::failed(format!("read task failed: {e}")))??;
    // The ledger cannot hash a whole file this large, so it records only the seen lines
    // (and the file's len/mtime); the path lock taken in `call_with` is still held here.
    let refs: Vec<&str> = selected.iter().map(String::as_str).collect();
    ctx.host
        .ledger_record_read_range(&ctx.agent, full_path, offset - 1, &refs)
        .await;
    let footer = (!request.ranged).then(|| {
        format!(
            "file is larger than the {MAX_READ_BYTES} byte read cap, so it was not read \
             whole; read on with offset={}, or search it with `grep` instead",
            offset + refs.len()
        )
    });
    Ok(render_lines(
        &refs,
        offset,
        args.anchors.unwrap_or(true),
        footer.as_deref(),
        &args.path,
        ctx,
        accept_large_output,
    ))
}

/// Reads lines `offset..offset+limit` (1-based) with the same line splitting as `str::lines`
/// (`\n`, and `\r\n` only when followed by `\n`). Lines before `offset` are skipped unallocated.
fn read_line_range(path: &Path, offset: usize, limit: usize) -> Result<Vec<String>, ToolError> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut index = 0usize;
    while out.len() < limit {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        index += 1;
        if index < offset {
            continue;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        let line = String::from_utf8(std::mem::take(&mut buf))
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;
        out.push(line);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;
