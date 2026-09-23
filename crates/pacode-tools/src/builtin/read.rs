//! `read`: file contents with line numbers.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

pub struct ReadTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Read a file. Returns lines prefixed with their 1-based number (`   12\\tcode`). \
         Use offset/limit for large files; output above the cap is cut head+tail with a \
         marker reporting the dropped size. Binary files are refused."
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

    /// Reads the file (tokio::fs), rejects files with a NUL byte in the first 8 KiB,
    /// applies offset/limit, formats `{:>6}\t{line}`, appends a `— N more lines;
    /// read with offset=` footer when `limit` cut the listing short, caps to
    /// `ctx.output_cap_chars` (unless `accept_large_output`), title `Read
    /// <path>`, preview = the numbered lines (bounded) so the transcript cell
    /// can show and expand them.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<ReadInput>(input)?;
        if args.path.is_empty() {
            return Err(ToolError::invalid("path cannot be empty"));
        }
        let full_path = ctx.resolve(Path::new(&args.path));
        let bytes = tokio::fs::read(&full_path).await?;
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed(format!(
                "cannot read binary file: {}",
                args.path
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("file is not valid UTF-8: {e}")))?;

        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(2000);

        let mut lines_out = Vec::new();
        let mut total_lines = 0usize;
        for (line_num, line) in (1..).zip(text.lines()) {
            total_lines = line_num;
            if line_num >= offset && lines_out.len() < limit {
                lines_out.push(format!("{line_num:>6}\t{line}"));
            }
        }

        let mut raw = lines_out.join("\n");
        // When `limit` cut the listing short, a footer names the remainder and
        // the `offset` that continues it, so the model can page on its own. The
        // char cap needs no hint — `cap_output` already reports its own cut.
        let next_offset = offset + lines_out.len();
        let remaining = total_lines.saturating_sub(next_offset.saturating_sub(1));
        if remaining > 0 {
            let noun = if remaining == 1 { "line" } else { "lines" };
            raw.push_str(&format!(
                "\n— {remaining} more {noun}; read with offset={next_offset}"
            ));
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
