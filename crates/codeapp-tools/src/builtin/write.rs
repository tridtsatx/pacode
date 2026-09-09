//! `write`: create or overwrite a file.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "write";

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content. Parent directories are \
         created. Prefer `edit` for changes to existing files."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Write <path>` with a unified diff against the current content
    /// (or `new file, N lines`) as detail; writes atomically (temp file + rename in the
    /// same dir); output `Wrote <bytes> bytes to <path>`; diff stat from
    /// `codeapp_render::diff_stat`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("WriteTool::call")
    }
}
