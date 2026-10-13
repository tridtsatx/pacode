//! `read`: file contents with line numbers.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::helpers::{cap_output, parse_input, preview_body};
use crate::host::PermissionDraft;
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

/// Model-facing description; see `prompts/read.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/read.md");

const DEFAULT_LINE_LIMIT: usize = 2000;
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Bytes of the file the scan walks. Past this the scan stops and says so: line
/// numbers are counted from the start, so a page deep in a huge file costs the
/// same as the first one, and the file cannot pin memory or the worker pool.
const MAX_READ_SCAN_BYTES: u64 = 8 * 1024 * 1024;

/// Bytes kept for the listing itself. The output cap trims it again, but a file
/// of 100 KB lines would otherwise be joined into one string first.
const MAX_READ_WINDOW_BYTES: usize = 1024 * 1024;

const BINARY_SNIFF_BYTES: usize = 8192;

#[derive(Default)]
pub struct ReadTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// What one pass over the file produced: the lines inside the requested window,
/// plus how many lines the file has in total (which is what the continuation
/// footer reports, and counting them all is why the scan is bounded rather than
/// the file being read whole).
struct Listing {
    lines: Vec<String>,
    total_lines: usize,
    window_capped: bool,
    scan_capped: bool,
}

impl Listing {
    fn push_line(
        &mut self,
        raw: &[u8],
        line_num: usize,
        offset: usize,
        limit: usize,
        window_bytes: &mut usize,
    ) -> Result<(), ToolError> {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let text = std::str::from_utf8(raw)
            .map_err(|err| ToolError::failed(format!("file is not valid UTF-8: {err}")))?;
        self.total_lines = line_num;
        if line_num < offset || self.lines.len() >= limit {
            return Ok(());
        }
        let numbered = format!("{line_num:>6}\t{text}");
        if *window_bytes + numbered.len() > MAX_READ_WINDOW_BYTES {
            self.window_capped = true;
            return Ok(());
        }
        *window_bytes += numbered.len();
        self.lines.push(numbered);
        Ok(())
    }
}

/// Walk `file` once, keeping the requested window and counting the rest. A
/// partial trailing line is only emitted when the scan actually reached the end
/// of the file, so a page cut by the scan cap never shows half a line.
async fn scan_listing(
    file: tokio::fs::File,
    offset: usize,
    limit: usize,
    label: &str,
) -> Result<Listing, ToolError> {
    let mut reader = tokio::io::BufReader::with_capacity(READ_CHUNK_BYTES, file);
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    let mut carry: Vec<u8> = Vec::new();
    let mut head: Vec<u8> = Vec::new();
    let mut listing = Listing {
        lines: Vec::new(),
        total_lines: 0,
        window_capped: false,
        scan_capped: false,
    };
    let mut line_num = 0usize;
    let mut window_bytes = 0usize;
    let mut scanned: u64 = 0;
    let mut at_end = false;

    while !at_end {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => {
                at_end = true;
                break;
            }
            Ok(read) => read,
            Err(err) => return Err(ToolError::Io(err)),
        };
        if scanned >= MAX_READ_SCAN_BYTES {
            listing.scan_capped = true;
            break;
        }
        let take = read.min((MAX_READ_SCAN_BYTES - scanned) as usize);
        if take < read {
            listing.scan_capped = true;
        }
        scanned += take as u64;

        if head.len() < BINARY_SNIFF_BYTES {
            let sniff = take.min(BINARY_SNIFF_BYTES - head.len());
            head.extend_from_slice(&chunk[..sniff]);
            if head.contains(&0) {
                return Err(ToolError::failed(format!(
                    "cannot read binary file: {label}"
                )));
            }
        }

        let mut start = 0usize;
        for index in 0..take {
            if chunk[index] != b'\n' {
                continue;
            }
            if carry.is_empty() {
                line_num += 1;
                listing.push_line(
                    &chunk[start..index],
                    line_num,
                    offset,
                    limit,
                    &mut window_bytes,
                )?;
            } else {
                carry.extend_from_slice(&chunk[start..index]);
                line_num += 1;
                listing.push_line(&carry, line_num, offset, limit, &mut window_bytes)?;
                carry.clear();
            }
            start = index + 1;
        }
        carry.extend_from_slice(&chunk[start..take]);
    }

    if at_end && !carry.is_empty() {
        line_num += 1;
        listing.push_line(&carry, line_num, offset, limit, &mut window_bytes)?;
    }
    Ok(listing)
}

#[async_trait]
impl Tool for ReadTool {
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
                "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                "offset": {"type": "integer", "minimum": 1, "description": "First line to return (1-based)."},
                "limit": {"type": "integer", "minimum": 1, "description": "Max lines to return (default 2000)."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// Scans the file in chunks (never loading it whole, and never opening
    /// anything but a regular file — a fifo would block on open), rejects files
    /// with a NUL byte in the first 8 KiB, applies offset/limit, formats
    /// `{:>6}\t{line}`, appends a `— N more lines; read with offset=` footer when
    /// the listing stopped short, caps to `ctx.output_cap_chars` (unless
    /// `accept_large_output`), title `Read <path>`, preview = the numbered lines
    /// (bounded) so the transcript cell can show and expand them.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
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
        // tool that otherwise reads a secret unprompted.
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

        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_LINE_LIMIT);

        let file = tokio::fs::File::open(&full_path).await?;
        let listing = scan_listing(file, offset, limit, &args.path).await?;

        let mut raw = listing.lines.join("\n");
        // When the listing stopped short, a footer names the remainder and the
        // `offset` that continues it, so the model can page on its own. The char
        // cap needs no hint — `cap_output` already reports its own cut.
        let next_offset = offset + listing.lines.len();
        let remaining = listing
            .total_lines
            .saturating_sub(next_offset.saturating_sub(1));
        let mut notes: Vec<String> = Vec::new();
        if remaining > 0 {
            let noun = if remaining == 1 { "line" } else { "lines" };
            notes.push(format!(
                "{remaining} more {noun}; read with offset={next_offset}"
            ));
        }
        if listing.window_capped {
            notes.push(format!(
                "listing stopped at the {MAX_READ_WINDOW_BYTES} byte read window"
            ));
        }
        if listing.scan_capped {
            notes.push(format!(
                "file is longer than the {MAX_READ_SCAN_BYTES} byte read cap, so nothing past \
                 it was read; search it with `grep` instead"
            ));
        }
        if !notes.is_empty() {
            raw.push_str(&format!("\n— {}", notes.join("; ")));
        }
        let content = cap_output(&raw, accept_large_output, ctx.output_cap_chars);
        let preview = preview_body(&content);
        let title = format!("Read {}", args.path);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;
