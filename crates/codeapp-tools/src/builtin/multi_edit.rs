//! `multi_edit`: several replacements in one file, all or nothing.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "multi_edit";

pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Apply several exact replacements to one file in order, atomically: if any \
         edit fails nothing is written. Same matching rules as `edit`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "edits"],
            "properties": {
                "path": {"type": "string"},
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": {"type": "string"},
                            "new_string": {"type": "string"},
                            "replace_all": {"type": "boolean", "default": false}
                        }
                    }
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Applies `edit::apply_replacement` sequentially in memory; one permission request
    /// with the combined diff; atomic write; output lists replacements per edit.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("MultiEditTool::call")
    }
}
