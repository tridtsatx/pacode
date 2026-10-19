//! `agent`: spawn and manage subagents (spec §8).

use async_trait::async_trait;
use pacode_types::AgentId;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::host::{AgentKindDef, AgentSpec};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "agent";

/// Longest `message` / `answer` text, in characters.
pub const MAX_MESSAGE_CHARS: usize = 4000;

pub struct AgentTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct AgentInput {
    action: String,
    prompt: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    context: Option<String>,
    tools: Option<Vec<String>>,
    agent_id: Option<String>,
    text: Option<String>,
    question_id: Option<String>,
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
         `agents/` dir whose file supplies the brief and tools — `list` shows \
         the available kinds. `stop` cancels, `ask_status` asks a running subagent for \
         a brief status that arrives at your next step. `message {agent_id, text}` \
         sends a running subagent an extra instruction, todo item or correction \
         (delivered between its steps; fails if it already finished). A subagent may \
         ask you a question: it arrives as `<question_from_agent id=\"q1\">`; reply \
         with `answer {agent_id, question_id, text}`. Prefer a reasonable assumption \
         over asking, ask only when blocked, and keep questions and answers short. \
         `result {agent_id}` returns a finished agent's final answer (use it when the \
         delivered report was cut short). \
         Subagents cannot spawn agents."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["spawn", "stop", "list", "ask_status", "message", "answer", "result"]},
                "prompt": {"type": "string", "description": "Full task description for the subagent (spawn)."},
                "kind": {"type": "string", "description": "Named subagent type from `agents/*.md` files; `list` shows what is available."},
                "name": {"type": "string", "description": "Short name shown in the UI, e.g. `tests`."},
                "context": {"type": "string", "enum": ["fresh", "fork"], "default": "fresh", "description": "`fork` copies your conversation so far."},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Restrict the subagent to these tools."},
                "agent_id": {"type": "string"},
                "text": {"type": "string", "description": "Message or answer text, max 4000 chars (message, answer)."},
                "question_id": {"type": "string", "description": "Id of the subagent's question, e.g. `q3` (answer)."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `spawn` → `host.spawn_agent(AgentSpec{..})` → output `Spawned agent <id>
    /// (<name>)`; a `kind` merges the discovered file's prompt/tools/model. The model
    /// comes only from the kind file; otherwise the session inherits the parent's.
    /// Any model/effort the LLM sends is ignored. An unknown `kind` fails listing
    /// the available kinds.
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
                // Model is never chosen by the LLM: only a kind file may pin one.
                // `None` lets the session inherit the parent's model.
                let model = match &kind {
                    Some(def) => match def.model.as_deref() {
                        Some(route) => {
                            Some(ctx.host.parse_model_route(route).ok_or_else(|| {
                                ToolError::invalid(format!(
                                    "agent kind '{}' sets an unresolvable model route '{route}'",
                                    def.name
                                ))
                            })?)
                        }
                        None => None,
                    },
                    None => None,
                };
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
                    // Kind files carry no effort; `None` inherits the session default.
                    effort: None,
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
            "message" => {
                let id = AgentId::new(
                    args.agent_id
                        .ok_or_else(|| ToolError::invalid("agent_id is required for message"))?,
                );
                let text = check_text(args.text, "message")?;
                ctx.host.message_agent(&id, text)?;
                Ok(ToolOutput::text(format!(
                    "Message queued for agent {id}; it reads it at its next step."
                ))
                .with_title(format!("Message {id}"))
                .with_preview("queued"))
            }
            "answer" => {
                let id = AgentId::new(
                    args.agent_id
                        .ok_or_else(|| ToolError::invalid("agent_id is required for answer"))?,
                );
                let question_id = args
                    .question_id
                    .map(|q| q.trim().to_string())
                    .filter(|q| !q.is_empty())
                    .ok_or_else(|| ToolError::invalid("question_id is required for answer"))?;
                let text = check_text(args.text, "answer")?;
                ctx.host.answer_agent(&id, &question_id, text)?;
                Ok(
                    ToolOutput::text(format!("Answer to {question_id} delivered to agent {id}."))
                        .with_title(format!("Answer {id}"))
                        .with_preview("delivered"),
                )
            }
            "result" => {
                let id = AgentId::new(
                    args.agent_id
                        .ok_or_else(|| ToolError::invalid("agent_id is required for result"))?,
                );
                let info = ctx
                    .host
                    .agent_info(&id)
                    .ok_or_else(|| ToolError::invalid(format!("unknown agent: {id}")))?;
                let content = match (&info.summary, &info.error) {
                    _ if info.status.is_live() => {
                        format!("Agent {id} is still running; its report arrives when it finishes.")
                    }
                    (Some(summary), _) if !summary.trim().is_empty() => {
                        cap_output(summary, accept_large_output, ctx.output_cap_chars)
                    }
                    (_, Some(error)) => format!("Agent {id} failed: {error}"),
                    _ => format!("Agent {id} produced no final answer."),
                };
                Ok(ToolOutput::text(content)
                    .with_title(format!("Agent result {id}"))
                    .with_preview(format!("{:?}", info.status).to_lowercase()))
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

/// Trim and validate the text of `message` / `answer`.
fn check_text(text: Option<String>, what: &str) -> Result<String, ToolError> {
    let text = text
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ToolError::invalid(format!("text is required for {what}")))?;
    let len = text.chars().count();
    if len > MAX_MESSAGE_CHARS {
        return Err(ToolError::invalid(format!(
            "text is {len} characters; the limit is {MAX_MESSAGE_CHARS}"
        )));
    }
    Ok(text)
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

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;
