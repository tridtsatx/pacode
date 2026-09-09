//! `agent`: spawn and manage subagents (spec §8).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "agent";

pub struct AgentTool;

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run work in parallel with subagents. `spawn` starts one with its own context \
         and returns immediately with an agent id; its final report is delivered to you \
         automatically when it finishes, so continue with other work. `wait` blocks \
         (bounded) only when you cannot proceed without the result. `stop` cancels, \
         `list` shows all agents. Subagents cannot spawn agents."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["spawn", "wait", "stop", "list"]},
                "prompt": {"type": "string", "description": "Full task description for the subagent (spawn)."},
                "name": {"type": "string", "description": "Short name shown in the UI, e.g. `tests`."},
                "model": {"type": "string", "description": "`provider/model` override."},
                "effort": {"type": "string", "enum": ["low", "medium", "high", "max"]},
                "context": {"type": "string", "enum": ["fresh", "fork"], "default": "fresh", "description": "`fork` copies your conversation so far."},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Restrict the subagent to these tools."},
                "agent_id": {"type": "string"},
                "timeout_secs": {"type": "integer", "minimum": 1, "default": 300}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `spawn` → `host.spawn_agent(AgentSpec{..})` → output `Spawned agent <id> (<name>)`;
    /// `wait` → `host.wait_agent` then the agent's summary/status; `stop`; `list` → one
    /// line per agent `<id> <name> <status> <duration> ↓<tokens>`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("AgentTool::call")
    }
}
