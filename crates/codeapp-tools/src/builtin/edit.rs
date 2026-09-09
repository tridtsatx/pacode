//! `edit`: exact string replacement.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "edit";

pub struct EditTool;

/// Apply one replacement to `content`. Errors: `old_string` not found, or found more
/// than once without `replace_all` (message says how many matches). Returns the new
/// content and the number of replacements.
pub fn apply_replacement(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    let _ = (content, old, new, replace_all);
    todo!("edit::apply_replacement")
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. `old_string` must occur exactly once \
         (include surrounding lines to make it unique) unless `replace_all` is true. \
         Read the file first so the match is exact."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean", "default": false}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    /// Permission title `Edit <path>`, detail = unified diff (context 3); atomic write;
    /// output `Edited <path>: N replacement(s)`, diff stat; preview = first changed line.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("EditTool::call")
    }
}
