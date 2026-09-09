//! `plan`: the session plan shown in the rail (spec §10).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "plan";

pub struct PlanTool;

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Maintain the visible plan for multi-step work. `set` replaces the whole list \
         (keep ids stable when re-setting); `update` changes one item's status and/or \
         progress. Exactly one item should be `active` at a time. Report `progress` \
         (0-100) only when you can measure it."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["set", "update", "get"]},
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["content"],
                        "properties": {
                            "id": {"type": "string"},
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "active", "done", "cancelled"], "default": "pending"},
                            "progress": {"type": "integer", "minimum": 0, "maximum": 100}
                        }
                    }
                },
                "item_id": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "active", "done", "cancelled"]},
                "progress": {"type": "integer", "minimum": 0, "maximum": 100}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `set`: build `Plan{version: old+1, items}` assigning ids `p1..pN` when missing;
    /// `update`: modify the item (unknown id → InvalidInput); `done` clears progress;
    /// `get`: render the plan. Output = rendered plan (`[x]`/`[*] 60%`/`[ ]` lines).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("PlanTool::call")
    }
}
