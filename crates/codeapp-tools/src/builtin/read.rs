//! `read`: file contents with line numbers.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Read a file. Returns lines prefixed with their 1-based number (`   12\\tcode`). \
         Use offset/limit for large files; output above the cap is cut head+tail and the \
         full size is reported. Binary files are refused."
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
    /// applies offset/limit, formats `{:>6}\t{line}`, caps to `ctx.output_cap_chars`
    /// (unless `accept_large_output`), title `Read <path>`, preview = line count.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("ReadTool::call")
    }
}
