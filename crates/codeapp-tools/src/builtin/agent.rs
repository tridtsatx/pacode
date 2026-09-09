//! `agent`: spawn and manage subagents (spec §8).

use std::time::Duration;

use async_trait::async_trait;
use codeapp_types::AgentId;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::host::AgentSpec;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WaitOutcome};

pub const NAME: &str = "agent";

pub struct AgentTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct AgentInput {
    action: String,
    prompt: Option<String>,
    name: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    context: Option<String>,
    tools: Option<Vec<String>>,
    agent_id: Option<String>,
    timeout_secs: Option<u64>,
}

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
        let (args, accept_large_output) = parse_input::<AgentInput>(input)?;
        match args.action.as_str() {
            "spawn" => {
                let prompt = args
                    .prompt
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("prompt is required for spawn"))?;
                let model = args
                    .model
                    .as_deref()
                    .and_then(codeapp_types::ModelRoute::parse_lossy);
                let effort = args
                    .effort
                    .as_deref()
                    .and_then(codeapp_types::Effort::parse);
                let fork = args.context.as_deref() == Some("fork");
                let spec = AgentSpec {
                    prompt,
                    name: args.name.clone(),
                    model,
                    effort,
                    fork,
                    tools: args.tools,
                };
                let id = ctx.host.spawn_agent(spec).await?;
                let agent_name = args.name.unwrap_or_else(|| id.to_string());
                let content = format!("Spawned agent {id} ({agent_name})");
                let title = format!("Spawn agent {agent_name}");
                Ok(ToolOutput::text(content)
                    .with_title(title)
                    .with_preview(format!("agent {id}")))
            }
            "wait" => {
                let agent_id_str = args
                    .agent_id
                    .ok_or_else(|| ToolError::invalid("agent_id is required for wait"))?;
                let id = AgentId::new(agent_id_str);
                let timeout_secs = args.timeout_secs.unwrap_or(300);
                let outcome = ctx
                    .host
                    .wait_agent(&id, Duration::from_secs(timeout_secs))
                    .await;
                match outcome {
                    WaitOutcome::Finished => {
                        let info = ctx.host.agent_info(&id);
                        let summary = info
                            .as_ref()
                            .and_then(|i| i.summary.clone())
                            .unwrap_or_else(|| {
                                info.as_ref()
                                    .map(|i| format!("status: {:?}", i.status))
                                    .unwrap_or_else(|| "finished".to_string())
                            });
                        let content = format!("Agent {id} finished. Summary: {summary}");
                        Ok(ToolOutput::text(content)
                            .with_title(format!("Agent wait {id}"))
                            .with_preview("finished"))
                    }
                    WaitOutcome::Timeout | WaitOutcome::Progress => {
                        let content = format!("Agent {id} is still running after {timeout_secs}s.");
                        Ok(ToolOutput::text(content)
                            .with_title(format!("Agent wait {id}"))
                            .with_preview("running"))
                    }
                    WaitOutcome::Cancelled => Err(ToolError::Cancelled),
                }
            }
            "stop" => {
                let agent_id_str = args
                    .agent_id
                    .ok_or_else(|| ToolError::invalid("agent_id is required for stop"))?;
                let id = AgentId::new(agent_id_str);
                ctx.host.stop_agent(&id).await?;
                let content = format!("Agent {id} stopped.");
                Ok(ToolOutput::text(content)
                    .with_title(format!("Agent stop {id}"))
                    .with_preview("stopped"))
            }
            "list" => {
                let agents = ctx.host.list_agents();
                if agents.is_empty() {
                    return Ok(ToolOutput::text("No subagents.").with_title("agent list"));
                }
                let now_ms = codeapp_types::now_ms();
                let mut lines = Vec::new();
                for a in &agents {
                    let status_str = format!("{:?}", a.status).to_lowercase();
                    let dur_s = a.duration_ms(now_ms) / 1000;
                    lines.push(format!(
                        "{} {} {} {}s ↓{}",
                        a.id, a.name, status_str, dur_s, a.tokens_out
                    ));
                }
                let joined = lines.join("\n");
                let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let preview = format!("{} agents", agents.len());
                Ok(ToolOutput::text(content)
                    .with_title("agent list")
                    .with_preview(preview))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}
