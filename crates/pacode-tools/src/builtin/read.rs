//! `read`: file contents with hashline anchors (`12#ab| code`) for the `edit` tool.

use std::path::Path;

use async_trait::async_trait;
use pacode_types::ReadConfig;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use super::read_outline::{ReadMode, SmartArgs, smart_read};
use crate::hashline::annotate_lines;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

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
        "Read a file. Each line is returned as `LINE#HASH| text` (1-based line, 2-char content \
         hash); `edit` addresses lines by these anchors, so copy them exactly and never \
         include them in new text. Lines over 2000 chars are shown cut but stay anchored. \
         Use offset/limit for large files; output above the cap is cut head+tail and the \
         full size is reported. `anchors:false` gives plain numbering. Binary files are refused. \
         Reading a file is what lets you change it later with `edit`/`write`. \
         Big source files come back as an outline (signatures, first body lines, closing \
         lines; `… N lines hidden (La-Lb) …` markers between): hidden lines have no anchors \
         and do not count as read, so before editing them show them with `symbol` \
         (`Foo::bar`) or `expand` (`[\"44-80\"]`); `mode:\"full\"` reads everything."
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
    /// Reads the file (tokio::fs), rejects files with a NUL byte in the first 8 KiB,
    /// answers outline / `symbol` / `expand` reads via `read_outline::smart_read`,
    /// else applies offset/limit, formats `LINE#HASH| line` (or `{:>6}\t{line}` with `anchors:false`), caps to `ctx.output_cap_chars`
    /// (unless `accept_large_output`), title `Read <path>`, preview = line count.
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
        // Under the path's write lock, so the ledger records the version actually read.
        let _lock = ctx.host.ledger_lock(std::slice::from_ref(&full_path)).await;
        let bytes = tokio::fs::read(&full_path).await?;
        if let Some(out) = super::read_image::image_output(&bytes, &args.path).await {
            return Ok(out);
        }
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed(format!(
                "cannot read binary file: {}",
                args.path
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;
        let request = SmartArgs {
            mode: match args.anchors {
                Some(false) => ReadMode::Full,
                _ => args.mode.unwrap_or_default(),
            },
            symbol: args.symbol.as_deref(),
            expand: &args.expand,
            ranged: args.offset.is_some() || args.limit.is_some(),
        };
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
        let limit = args.limit.unwrap_or(2000);

        let selected: Vec<&str> = text.lines().skip(offset - 1).take(limit).collect();
        let shown = (offset - 1)..(offset - 1 + selected.len());
        ctx.host
            .ledger_record_read(&ctx.agent, &full_path, &text, shown)
            .await;
        let raw = if args.anchors.unwrap_or(true) {
            annotate_lines(selected.iter().copied(), offset)
        } else {
            selected
                .iter()
                .enumerate()
                .map(|(k, line)| format!("{:>6}\t{line}", offset + k))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let content = cap_output(&raw, accept_large_output, ctx.output_cap_chars);
        let preview = format!("{} lines", selected.len());
        let title = format!("Read {}", args.path);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}
