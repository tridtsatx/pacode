//! `ls`: directory listing.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "ls";

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "List a directory: one entry per line, directories with a trailing `/`, files \
         with their size. Hidden entries only with `all`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory (default: working directory)."},
                "all": {"type": "boolean", "default": false}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `tokio::fs::read_dir`; sorted directories first then files, alphabetical; cap
    /// 500 entries with a `… N more` line.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("LsTool::call")
    }
}
