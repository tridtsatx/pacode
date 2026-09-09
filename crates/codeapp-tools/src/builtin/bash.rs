//! `bash`: run a shell command as a session task (spec §9).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "bash";

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run a shell command in the working directory. Long commands (builds, tests, \
         servers) should use `background: true`, or they are moved to the background \
         automatically after a few seconds: you then get a task id, keep working, and \
         a notification arrives when the task finishes. Use `bg` to check, wait for, \
         or kill tasks. Output above the cap is cut head+tail."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string", "description": "Command line for `sh -c`."},
                "background": {"type": "boolean", "default": false, "description": "Return a task id immediately instead of waiting."},
                "timeout_secs": {"type": "integer", "minimum": 1, "description": "Kill the command after this many seconds (default: none for background, 3600 otherwise)."},
                "label": {"type": "string", "description": "Short label for the task list, e.g. `cargo test`."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    /// 1. `codeapp_risk::classify(command, cwd)`; in `Mode::Plan` the command must also
    ///    satisfy `codeapp_risk::is_read_only`, else `ToolError::Denied` without prompting.
    /// 2. `ctx.require_permission("Bash: <first 60 chars>", command + risk summary, Some(risk))`
    ///    (the host applies the matrix; Safe returns Allow without asking).
    /// 3. `host.spawn_task(TaskSpec{...})`; `background: true` → return
    ///    `{task_id, status: running}` immediately, title `Bash <label>`,
    ///    `ToolOutput.task = Some(id)`.
    /// 4. Foreground: `host.wait_task(id, yield_after, false)`; `Finished` → output from
    ///    `host.task_tail`/full output (capped) + exit code line; `Timeout` → the task
    ///    keeps running: output `Command still running after Ns; moved to background as
    ///    task <id>. Latest output:\n<tail>`, `ToolOutput.task = Some(id)`.
    ///    The core marks the task backgrounded when `task` is set on a foreground call.
    /// `yield_after` comes from `ctx` (`ToolCtx::exec_yield_after`, added below).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("BashTool::call")
    }
}
