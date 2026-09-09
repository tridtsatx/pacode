//! `bg`: manage background tasks.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "bg";

pub struct BgTool;

#[async_trait]
impl Tool for BgTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Background tasks: `list` running/finished tasks, `status` of one, `tail` its \
         last lines, `wait` until it finishes (bounded), `kill` it, or report \
         `progress` (current/total or percent) for a task you are supervising. You do \
         not need to wait: task completion is delivered to you automatically."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["list", "status", "tail", "wait", "kill", "progress"]},
                "task_id": {"type": "string"},
                "lines": {"type": "integer", "minimum": 1, "default": 80},
                "timeout_secs": {"type": "integer", "minimum": 1, "default": 60, "description": "For wait; max 3600."},
                "current": {"type": "integer"},
                "total": {"type": "integer"},
                "percent": {"type": "number"},
                "message": {"type": "string"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `list`: one line per task `<id> <status> <label> <duration> [<progress>]`;
    /// `status`: full TaskInfo lines; `tail`: last N lines; `wait`: `host.wait_task`
    /// with `return_on_progress = true` (reports progress and remaining state);
    /// `kill`: `host.kill_task`; `progress`: `host.report_task_progress` with source
    /// Reported. Missing task_id where required → InvalidInput.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("BgTool::call")
    }
}
