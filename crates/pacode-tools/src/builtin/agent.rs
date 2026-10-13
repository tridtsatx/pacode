//! `agent`: spawn and manage subagents (spec §8).

use async_trait::async_trait;
use pacode_types::{AgentId, QuestionOption};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::host::{AgentKindDef, AgentSpec};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "agent";

/// How much of the orchestrator's message the user sees when asked whether
/// to resume an agent they stopped.
const MESSAGE_PREVIEW_CHARS: usize = 300;

/// Model-facing description; see `prompts/agent.md`.
const DESCRIPTION: &str = include_str!("../prompts/agent.md");

#[derive(Default)]
pub struct AgentTool {
    definition: DefinitionCache,
}

/// What `agent kinds` / `agent list` says when no kinds are defined.
///
/// This text is the whole reason `kinds` exists as a separate action: the tool
/// *description* cannot list the kinds, because [`Tool::description`] is served
/// from a per-tool cache and has no [`ToolCtx`] to discover them with — a
/// dynamically-built description would either be built once for the process
/// (stale the moment a kind file appears) or rebuilt on every model step, which
/// is a directory scan per step. So the list is read at *call* time, where the
/// host is reachable, and the description points at it. An explicit
/// `kinds` action is cheap (a directory listing) and has no side effects, so
/// the model can ask for it at the start of a task instead of guessing.
const NO_KINDS_HINT: &str = "No agent kinds are defined — omit `kind` and the subagent gets the \
     default tool set. `kind` only works for types declared in `agents/*.md`.";

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
    exclude_tools: Option<Vec<String>>,
    agent_id: Option<String>,
    message: Option<String>,
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION.trim()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["spawn", "send", "stop", "continue", "list", "kinds", "ask_status"]},
                "message": {"type": "string", "description": "Text for the subagent (send)."},
                "prompt": {"type": "string", "description": "Full task description for the subagent (spawn)."},
                "kind": {"type": "string", "description": "Named subagent type from `agents/*.md` files; `kinds` shows what this session has."},
                "name": {"type": "string", "description": "Short name shown in the UI, e.g. `tests`."},
                "model": {"type": "string", "description": "`provider/model` override."},
                "effort": {"type": "string", "enum": ["low", "medium", "high", "max"]},
                "context": {"type": "string", "enum": ["fresh", "fork"], "default": "fresh", "description": "`fork` copies your conversation so far."},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Narrow the subagent to exactly these tools. Omit to give it the same set you have."},
                "exclude_tools": {"type": "array", "items": {"type": "string"}, "description": "Drop these tools, keeping the rest. Cheaper than `tools` when you only need to trim: every tool schema is re-sent on every step of every subagent, so a subagent that edits files gains nothing from the browser tools. Applied after `tools`; `agent` is never grantable."},
                "agent_id": {"type": "string"}
            }
        })
    }

    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `spawn` → `host.spawn_agent(AgentSpec{..})` → output `Spawned agent <id>
    /// (<name>)`; a `kind` merges the discovered file's prompt/tools/model unless the
    /// call overrides them. An unknown `kind` is a note, not a refusal: the
    /// spawn goes ahead on the default tool set and the output names what was
    /// ignored and which kinds do exist.
    /// `stop`; `list` → one line per agent `<id> <name> <status> <duration>
    /// ↓<tokens>` plus the available kinds.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.call_inner(input, ctx, std::time::Instant::now()).await
    }
}

impl AgentTool {
    /// The body of [`Tool::call`]. `started` is stamped by the caller so the
    /// `resolve` stage can be cut at the handoff rather than after it, which is
    /// the only way the stage can be smaller than the call that contains it.
    async fn call_inner(
        &self,
        input: Value,
        ctx: &ToolCtx,
        started: std::time::Instant,
    ) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<AgentInput>(input)?;
        match args.action.as_str() {
            "spawn" => {
                let prompt = args
                    .prompt
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("prompt is required for spawn"))?;
                let mut kind_note = None;
                let kind = match args
                    .kind
                    .as_deref()
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                {
                    Some(kind_name) => {
                        let (found, available) = resolve_kind(ctx, kind_name);
                        if found.is_none() {
                            kind_note = Some(format!(
                                "Note: agent kind '{kind_name}' is not defined, so this \
                                 subagent got the default tool set. Available kinds: {available}"
                            ));
                        }
                        found
                    }
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
                    exclude_tools: args.exclude_tools.clone(),
                };
                // `resolve` stops here, at the handoff — not after it. Wrapping
                // the whole call would make this stage *larger* than the
                // `ready` stage that contains it, and a stage that exceeds its
                // own total is not a measurement.
                pacode_types::record_launch_resolve(started.elapsed());
                let id = ctx.host.spawn_agent(spec).await?;
                let agent_name = args
                    .name
                    .or_else(|| kind.map(|k| k.name))
                    .unwrap_or_else(|| id.to_string());
                let mut content = format!("Spawned agent {id} ({agent_name})");
                if let Some(note) = kind_note {
                    content.push_str("\n\n");
                    content.push_str(&note);
                }
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
            "send" => {
                let id = AgentId::new(
                    args.agent_id
                        .ok_or_else(|| ToolError::invalid("agent_id is required for send"))?,
                );
                let message = args
                    .message
                    .filter(|m| !m.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("message is required for send"))?;
                let consent = user_consent(ctx, &id, Some(&message)).await?;
                let outcome = ctx.host.send_to_agent(&id, message, consent).await?;
                Ok(ToolOutput::text(outcome.describe(&id))
                    .with_title(format!("Agent send {id}"))
                    .with_preview(outcome.preview()))
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
            "continue" => {
                let agent_id_str = args
                    .agent_id
                    .ok_or_else(|| ToolError::invalid("agent_id is required for continue"))?;
                let id = AgentId::new(agent_id_str);
                let consent = user_consent(ctx, &id, None).await?;
                ctx.host.continue_agent(&id, consent).await?;
                let content = format!("Agent {id} resumed.");
                Ok(ToolOutput::text(content)
                    .with_title(format!("Agent continue {id}"))
                    .with_preview("resumed"))
            }
            "kinds" => {
                // The side-effect-free half of `list`: a directory listing, so
                // the model can ask what kinds exist without also asking for a
                // status table of agents it may not have spawned yet.
                let kinds = ctx.host.agent_kinds();
                let mut content = render_kinds(&kinds);
                content = cap_output(&content, accept_large_output, ctx.output_cap_chars);
                let preview = preview_body(&content);
                Ok(ToolOutput::text(content)
                    .with_title("agent kinds")
                    .with_preview(preview))
            }
            "list" => {
                let agents = ctx.host.list_agents();
                let kinds = ctx.host.agent_kinds();
                if agents.is_empty() && kinds.is_empty() {
                    // The hint about `kind` belongs here too: this is the state
                    // a fresh session starts in, and returning a bare "No
                    // subagents." is what left the model inventing kind names.
                    return Ok(
                        ToolOutput::text(format!("No subagents.\n\n{}", NO_KINDS_HINT))
                            .with_title("agent list"),
                    );
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
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(render_kinds(&kinds));
                let joined = lines.join("\n");
                let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let preview = preview_body(&content);
                Ok(ToolOutput::text(content)
                    .with_title("agent list")
                    .with_preview(preview))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}

/// The kind list both `kinds` and `list` print, read fresh from the host on
/// every call — this is the dynamic surface the static description cannot be.
///
/// The empty case is named rather than left blank. Silence here is what had the
/// model guessing whether `kind` was a usable argument at all, and it kept
/// inventing plausible names (`explore`, `reviewer`) to retry.
fn render_kinds(kinds: &[AgentKindDef]) -> String {
    if kinds.is_empty() {
        return NO_KINDS_HINT.to_string();
    }
    let mut lines = vec!["Available kinds:".to_string()];
    for k in kinds {
        let desc: String = k.description.chars().take(120).collect();
        if desc.is_empty() {
            lines.push(format!("  {}", k.name));
        } else {
            lines.push(format!("  {} — {desc}", k.name));
        }
    }
    lines.join("\n")
}

/// Look up `name` among the host's discovered agent kinds, returning it
/// alongside the names that do exist.
///
/// A name that isn't there is deliberately *not* an error. pacode ships no
/// built-in kinds — every one comes from an `agents/*.md` file — while the
/// tool schema takes `kind` as a free-form string, so a model reaching for a
/// plausible one (`explore`, `reviewer`) is expected, not a mistake. Failing
/// the spawn outright cost several turns of the model retrying the same name
/// before it gave up, which read as "subagents are slow to start". Returning
/// `None` lets the spawn proceed on the default tool set while the output
/// tells the model what was ignored and what does exist, so it can correct
/// itself in one step.
fn resolve_kind(ctx: &ToolCtx, name: &str) -> (Option<AgentKindDef>, String) {
    let kinds = ctx.host.agent_kinds();
    let found = kinds.iter().find(|k| k.name == name).cloned();
    let available = if kinds.is_empty() {
        "(none)".to_string()
    } else {
        kinds
            .iter()
            .map(|k| k.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    (found, available)
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;

/// Leave to resume `id` when the user stopped it: the user is asked directly
/// — a question, not a permission, so no mode answers it for them. `None`
/// when the user did not stop it; an error when they keep it stopped or do
/// not answer. `message` is what the orchestrator is about to send it.
async fn user_consent(
    ctx: &ToolCtx,
    id: &AgentId,
    message: Option<&str>,
) -> Result<Option<u64>, ToolError> {
    let Some(stop) = ctx.host.user_stop(id) else {
        return Ok(None);
    };
    let name = &stop.name;
    let mut question = format!("Resume {name}? You stopped it.");
    if let Some(message) = message {
        let shown = pacode_types::truncate_head_tail(message.trim(), MESSAGE_PREVIEW_CHARS);
        question.push_str(&format!("\n\nThe orchestrator wants to send it:\n{shown}"));
    }
    let options = vec![
        QuestionOption::new("Resume", "Lift your stop; it goes on with its context"),
        QuestionOption::new("Keep stopped", "It stays stopped; the orchestrator is told"),
    ];
    let answer = ctx
        .host
        .ask_question(
            &ctx.call_id,
            "Resume agent".into(),
            question,
            options,
            false,
        )
        .await?;
    if answer.cancelled {
        return Err(ToolError::failed(format!(
            "the user did not answer; {name} ({id}) stays stopped"
        )));
    }
    if answer.selected.first() == Some(&0) {
        return Ok(Some(stop.stop));
    }
    let mut refusal = format!("the user chose to keep {name} ({id}) stopped");
    if let Some(said) = answer.free_text.as_deref().map(str::trim)
        && !said.is_empty()
    {
        refusal.push_str(&format!("; they said: {said}"));
    }
    Err(ToolError::failed(refusal))
}
