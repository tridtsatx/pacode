//! `agent`: spawn and manage subagents (spec §8).

use async_trait::async_trait;
use pacode_types::AgentId;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::host::{AgentKindDef, AgentSpec};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "agent";

pub struct AgentTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct AgentInput {
    action: String,
    prompt: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    context: Option<String>,
    tools: Option<Vec<String>>,
    agent_id: Option<String>,
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run work in parallel with subagents. `spawn` starts one with its own context \
         and returns immediately with an agent id; the finished agent's report is \
         delivered to you automatically as a message, so continue with other work or \
         end your turn rather than polling. `spawn` takes an optional `kind`: a named \
         subagent type from `<cwd>/.pacode/agents/*.md` or the global config's \
         `agents/` dir whose file supplies the brief, tools, and model — `list` shows \
         the available kinds. `stop` cancels, `ask_status` asks a running subagent for \
         a brief status that arrives at your next step. Subagents cannot spawn agents."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["spawn", "stop", "list", "ask_status"]},
                "prompt": {"type": "string", "description": "Full task description for the subagent (spawn)."},
                "kind": {"type": "string", "description": "Named subagent type from `agents/*.md` files; `list` shows what is available."},
                "name": {"type": "string", "description": "Short name shown in the UI, e.g. `tests`."},
                "model": {"type": "string", "description": "`provider/model` override."},
                "effort": {"type": "string", "enum": ["low", "medium", "high", "max"]},
                "context": {"type": "string", "enum": ["fresh", "fork"], "default": "fresh", "description": "`fork` copies your conversation so far."},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Restrict the subagent to these tools."},
                "agent_id": {"type": "string"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `spawn` → `host.spawn_agent(AgentSpec{..})` → output `Spawned agent <id>
    /// (<name>)`; a `kind` merges the discovered file's prompt/tools/model unless the
    /// call overrides them, an unknown `kind` fails listing the available kinds.
    /// `stop`; `list` → one line per agent `<id> <name> <status> <duration>
    /// ↓<tokens>` plus the available kinds.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<AgentInput>(input)?;
        match args.action.as_str() {
            "spawn" => {
                let prompt = args
                    .prompt
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("prompt is required for spawn"))?;
                let kind = match args
                    .kind
                    .as_deref()
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                {
                    Some(kind_name) => Some(resolve_kind(ctx, kind_name)?),
                    None => None,
                };
                let mut model = args
                    .model
                    .as_deref()
                    .and_then(pacode_types::ModelRoute::parse_lossy);
                if model.is_none()
                    && let Some(def) = &kind
                    && let Some(route) = def.model.as_deref()
                {
                    model = Some(ctx.host.parse_model_route(route).ok_or_else(|| {
                        ToolError::invalid(format!(
                            "agent kind '{}' sets an unresolvable model route '{route}'",
                            def.name
                        ))
                    })?);
                }
                let effort = args.effort.as_deref().and_then(pacode_types::Effort::parse);
                let fork = args.context.as_deref() == Some("fork");
                // The kind file's body is the subagent's brief; the call's prompt
                // is the concrete task, appended after it.
                let prompt = match kind.as_ref().map(|k| k.prompt.trim()) {
                    Some(brief) if !brief.is_empty() => format!("{brief}\n\n{prompt}"),
                    _ => prompt,
                };
                let spec = AgentSpec {
                    prompt,
                    name: args
                        .name
                        .clone()
                        .or_else(|| kind.as_ref().map(|k| k.name.clone())),
                    model,
                    effort,
                    fork,
                    tools: args
                        .tools
                        .clone()
                        .or_else(|| kind.as_ref().and_then(|k| k.tools.clone())),
                };
                let id = ctx.host.spawn_agent(spec).await?;
                let agent_name = args
                    .name
                    .or_else(|| kind.map(|k| k.name))
                    .unwrap_or_else(|| id.to_string());
                let content = format!("Spawned agent {id} ({agent_name})");
                let title = format!("Spawn agent {agent_name}");
                Ok(ToolOutput::text(content)
                    .with_title(title)
                    .with_preview(format!("agent {id}")))
            }
            "ask_status" => {
                let agent_id_str = args
                    .agent_id
                    .ok_or_else(|| ToolError::invalid("agent_id is required for ask_status"))?;
                let id = AgentId::new(agent_id_str);
                ctx.host.request_agent_status(&id)?;
                Ok(ToolOutput::text(format!(
                    "Status requested from agent {id}; the answer arrives at your next step."
                ))
                .with_title(format!("Agent status {id}"))
                .with_preview("requested"))
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
                let kinds = ctx.host.agent_kinds();
                if agents.is_empty() && kinds.is_empty() {
                    return Ok(ToolOutput::text("No subagents.").with_title("agent list"));
                }
                let now_ms = pacode_types::now_ms();
                let mut lines = Vec::new();
                for a in &agents {
                    let status_str = format!("{:?}", a.status).to_lowercase();
                    let dur_s = a.duration_ms(now_ms) / 1000;
                    lines.push(format!(
                        "{} {} {} {}s ↓{}",
                        a.id, a.name, status_str, dur_s, a.tokens_out
                    ));
                }
                if !kinds.is_empty() {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push("Available kinds:".to_string());
                    for k in &kinds {
                        let desc: String = k.description.chars().take(120).collect();
                        lines.push(format!("  {} — {desc}", k.name));
                    }
                }
                let joined = lines.join("\n");
                let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let preview = format!("{} agents, {} kinds", agents.len(), kinds.len());
                Ok(ToolOutput::text(content)
                    .with_title("agent list")
                    .with_preview(preview))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}

/// Look up `name` among the host's discovered agent kinds; an unknown name
/// fails with the available list so the model can self-correct.
fn resolve_kind(ctx: &ToolCtx, name: &str) -> Result<AgentKindDef, ToolError> {
    let kinds = ctx.host.agent_kinds();
    if let Some(def) = kinds.iter().find(|k| k.name == name) {
        return Ok(def.clone());
    }
    let available = if kinds.is_empty() {
        "(none)".to_string()
    } else {
        kinds
            .iter()
            .map(|k| k.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(ToolError::invalid(format!(
        "unknown agent kind '{name}', available kinds: {available}"
    )))
}
