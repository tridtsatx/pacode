//! `glob`: find files by pattern, newest first.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "glob";

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. `src/**/*.rs`), most recently \
         modified first, respecting .gitignore."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Root directory (default: working directory)."},
                "max_results": {"type": "integer", "minimum": 1, "default": 200}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `globset::Glob` matched against paths relative to the root while walking with
    /// `ignore::WalkBuilder`; collect (mtime, path); sort desc by mtime; cap; output one
    /// relative path per line; `spawn_blocking`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("GlobTool::call")
    }
}
