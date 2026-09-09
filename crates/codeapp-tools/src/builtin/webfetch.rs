//! `webfetch`: fetch a URL as text.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "webfetch";

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Fetch an http(s) URL and return its text (HTML converted to plain text). \
         Output above the cap is cut head+tail."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer", "minimum": 100}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    /// reqwest GET with 20 s timeout, follows ≤ 5 redirects, refuses non-http(s)
    /// schemes and bodies over 5 MiB; `text/html` → `html2text::from_read` at width 100;
    /// other text types verbatim; binary → error. Title `Fetch <host>`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("WebFetchTool::call")
    }
}
