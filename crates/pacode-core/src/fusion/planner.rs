//! The planner half of a fusion turn: a read-only model turn that ends in a
//! plan instead of an edit.
//!
//! It runs on **scratch history** — a copy of the agent's history at the moment
//! the planner started, plus its own rounds — and on the read-only registry
//! built from [`ALLOWED_TOOLS`]. Nothing it produces is persisted: the plan is
//! the only artifact that crosses back, so the implementer's context carries a
//! brief rather than the expensive model's whole exploration, and the resumed
//! transcript is exactly the live one.
//!
//! The caps, since this is a second conversation inside one turn: at most
//! [`MAX_STEPS`] model calls, tool results capped like every other tool result,
//! and a plan capped again at [`super::PLAN_CAP_CHARS`] before it is stored.
//! The turn's own tool-call budget does not meter the planner — the planner
//! runs before that meter matters to the implementer, and `MAX_STEPS` is the
//! bound that keeps it to a handful of reads. The scratch history is dropped
//! when the planner returns, whatever happened.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use pacode_provider::{CompletionRequest, EventStream, Provider};
use pacode_tools::ToolRegistry;
use pacode_tools::registry::{MAX_TOOL_OUTPUT_BYTES, cap_output_bytes};
use pacode_types::{CallId, ContentBlock, Event, Message, Role, Usage};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;
use crate::turn::stream::PendingToolCall;

use super::{FusionRoutes, PlanArtifact, PlanOutcome};

/// The planner's whole tool set. A name allowlist, not a kind filter: `batch`
/// is filed as read-only but runs `bash` ops, and a plugin or MCP tool can claim
/// any kind it likes. Names are the only thing here that cannot be argued with.
pub(crate) const ALLOWED_TOOLS: &[&str] = &["read", "ls", "glob", "grep", "symbols", "websearch"];

/// Model calls one planner turn may spend. Past this the planner has not
/// converged on a plan and the expensive model is being used as a very slow
/// `grep`; the turn degrades to the primary rather than billing more.
const MAX_STEPS: usize = 8;

/// What goes back to a planner that asked for a tool it does not have. Explicit
/// about *why* and about where the work goes, so the planner answers with a
/// plan instead of retrying the call.
pub(crate) fn refusal_text(name: &str) -> String {
    format!(
        "refused: `{name}` is not available in the planning phase. The planner may only read \
         ({}). Do not make the change — describe it in the plan; the implementer model applies it.",
        ALLOWED_TOOLS.join(", ")
    )
}

/// Appended to the shared static prompt. The planner keeps the repository
/// context it inherits and loses only the means to change anything.
const ROLE_DIRECTIVE: &str = "\n\n<role>\n\
You are the PLANNER half of a two-model turn. Read the repository as much as you need \
(you have read-only tools), then answer with the plan and nothing else: what to change, \
in which files, and how to verify it. You cannot edit, write or run anything — a \
reviewer applies your plan. No preamble, no questions to the user, no summary of what you \
read: the plan is the whole of your output.\n\
</role>";

/// One collected model step. Nothing here reaches the transcript: the planner
/// is not the user's answer and its exploration is not the session's history.
struct Step {
    text: String,
    reasoning: String,
    signature: Option<String>,
    calls: Vec<PendingToolCall>,
    usage: Usage,
    interrupted: bool,
}

/// Drain one planner stream without touching the agent's transcript. The turn
/// loop's consumer cannot be reused here: it creates live cells for every text
/// delta and every call, and the planner's output must leave no trace.
async fn collect(mut stream: EventStream, cancel: &CancellationToken) -> Result<Step, String> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut signature = None;
    let mut usage = Usage::default();
    let mut building: BTreeMap<u32, BuildingCall> = BTreeMap::new();

    let interrupted = loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => break true,
            next = stream.next() => next,
        };
        let Some(event) = next else { break false };
        match event {
            Ok(pacode_types::StreamEvent::TextDelta { text: delta }) => text.push_str(&delta),
            Ok(pacode_types::StreamEvent::ReasoningDelta { text: delta }) => {
                reasoning.push_str(&delta);
            }
            Ok(pacode_types::StreamEvent::ReasoningSignature {
                signature: sig,
                kind: _,
            }) => {
                signature = Some(sig);
            }
            Ok(pacode_types::StreamEvent::ToolCallStart { index, id, name }) => {
                // A retry re-issuing the same id under a new index must not
                // leave the first attempt behind to dispatch twice.
                building.retain(|_, call| call.call_id != id);
                building.insert(
                    index,
                    BuildingCall {
                        call_id: id,
                        name,
                        args: String::new(),
                        args_capped: false,
                    },
                );
            }
            Ok(pacode_types::StreamEvent::ToolCallArgsDelta { index, delta }) => {
                if let Some(call) = building.get_mut(&index) {
                    // Same cap as the turn loop's collector: past it the buffer
                    // is a JSON prefix, so it must stop growing and be
                    // reported as unparseable rather than as malformed input.
                    if call.args.len() + delta.len() > crate::turn::stream::RAW_ARGS_CAP {
                        call.args_capped = true;
                    } else {
                        call.args.push_str(&delta);
                    }
                }
            }
            Ok(pacode_types::StreamEvent::MessageStart { .. })
            | Ok(pacode_types::StreamEvent::MessageEnd { .. }) => {}
            Ok(pacode_types::StreamEvent::Usage(observed)) => usage = observed,
            Err(error) => return Err(error.to_string()),
        }
    };

    let calls = building
        .into_values()
        .map(|call| {
            let (parse_err, input) = crate::turn::stream::finish_args(&call.args, call.args_capped);
            PendingToolCall {
                call_id: call.call_id,
                name: call.name,
                input,
                parse_err,
                item_seq: None,
            }
        })
        .collect();
    Ok(Step {
        text,
        reasoning,
        signature,
        calls,
        usage,
        interrupted,
    })
}

/// A planner tool call still streaming. No transcript cell: the planner's
/// calls are not the user's transcript.
struct BuildingCall {
    call_id: CallId,
    name: String,
    args: String,
    args_capped: bool,
}

/// Run the planner. Returns [`PlanOutcome::Planned`] only with a non-empty
/// plan; every other exit is a [`PlanOutcome::Skipped`] the caller degrades on,
/// and none of them has written anything.
pub(crate) async fn run(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    provider: Arc<dyn Provider>,
    routes: &FusionRoutes,
    system_static: &str,
    system_dynamic: &str,
) -> PlanOutcome {
    let registry = agent
        .tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .subset(ALLOWED_TOOLS.iter().copied());

    // Which model is running, on the channel the client already renders.
    agent.set_status(
        pacode_types::AgentStatus::Thinking,
        Some(super::planner_activity(&routes.planner)),
    );
    session.events.emit(Event::AgentUpdated(agent.info()));

    let planner_static = format!("{system_static}{ROLE_DIRECTIVE}");
    let dynamic = system_dynamic.to_string();
    let effort = agent.info().effort;
    let mut scratch = agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .for_model();
    let mut usage = Usage::default();

    for _ in 0..MAX_STEPS {
        if cancel.is_cancelled() {
            return spent(usage, "interrupted during planning");
        }
        let request = CompletionRequest {
            model: routes.planner.model.clone(),
            system_static: planner_static.clone(),
            system_dynamic: dynamic.clone(),
            messages: crate::agent::sanitize_tool_pairing(&scratch).0,
            tools: registry.definitions(),
            effort: Some(effort),
            max_output_tokens: None,
        };
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return spent(usage, "interrupted during planning"),
            result = provider.complete(request) => match result {
                Ok(stream) => stream,
                Err(error) => return spent(usage, format!("planner call failed: {error}")),
            },
        };
        let step = match collect(stream, cancel).await {
            Ok(step) => step,
            Err(error) => return spent(usage, format!("planner stream failed: {error}")),
        };
        usage += step.usage;
        if step.interrupted {
            return spent(usage, "interrupted during planning");
        }

        let mut blocks = Vec::new();
        if !step.reasoning.is_empty() || step.signature.is_some() {
            blocks.push(ContentBlock::Reasoning {
                text: step.reasoning,
                signature: step.signature,
            });
        }
        if !step.text.is_empty() {
            blocks.push(ContentBlock::Text {
                text: step.text.clone(),
            });
        }
        for call in &step.calls {
            blocks.push(ContentBlock::ToolUse {
                id: call.call_id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }
        scratch.push(Arc::new(Message::new(Role::Assistant, blocks)));

        if step.calls.is_empty() {
            if step.text.trim().is_empty() {
                return spent(usage, "planner returned no plan");
            }
            return PlanOutcome::Planned(PlanArtifact {
                route: routes.planner.clone(),
                text: step.text,
                usage,
            });
        }

        let (results, interrupted) = run_calls(session, agent, cancel, &registry, step.calls).await;
        if interrupted {
            return spent(usage, "interrupted during planning");
        }
        scratch.push(Arc::new(Message::new(Role::Tool, results)));
    }

    spent(
        usage,
        format!("planner spent {MAX_STEPS} calls without producing a plan"),
    )
}

/// A planner that stopped without a plan, carrying what it had already spent.
fn spent(usage: Usage, reason: impl Into<String>) -> PlanOutcome {
    PlanOutcome::Skipped {
        reason: reason.into(),
        usage,
    }
}

/// Dispatch the planner's calls. Reads run concurrently like the turn loop's
/// `ReadOnly` group; a denied or unknown name gets the refusal as a tool
/// result, which is what lets the planner turn continue instead of dying on
/// its own mistake.
async fn run_calls(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    registry: &ToolRegistry,
    calls: Vec<PendingToolCall>,
) -> (Vec<ContentBlock>, bool) {
    let futures = calls.into_iter().map(|call| {
        let registry = registry.clone();
        let session = session.clone();
        let agent = agent.clone();
        let cancel = cancel.clone();
        async move { run_one(&session, &agent, &cancel, &registry, call).await }
    });
    let mut results = Vec::new();
    let mut interrupted = false;
    for outcome in futures::future::join_all(futures).await {
        interrupted |= outcome.1;
        results.push(outcome.0);
    }
    (results, interrupted)
}

async fn run_one(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    registry: &ToolRegistry,
    call: PendingToolCall,
) -> (ContentBlock, bool) {
    let result = |content: String, is_error: bool| {
        (
            ContentBlock::ToolResult {
                call_id: call.call_id.clone(),
                content,
                is_error,
            },
            false,
        )
    };
    let refused = |content: String| result(content, true);

    // The request already carried the allowlist, so a write call here is the
    // model ignoring its tools, not a race. Refuse it explicitly.
    if !ALLOWED_TOOLS.contains(&call.name.as_str()) {
        return refused(refusal_text(&call.name));
    }
    let Some(tool) = registry.get(&call.name) else {
        return refused(format!(
            "refused: `{name}` is not registered.",
            name = call.name
        ));
    };
    let kind = tool.kind();
    if let Some(reason) = crate::permissions::zdr_tool_violation(session, &call.name, kind) {
        return refused(reason);
    }
    // The allowlist is read-only, which every mode allows; the gate is read
    // anyway so a future entry here cannot quietly skip the matrix.
    if let crate::permissions::GateDecision::Deny(reason) =
        crate::permissions::gate(session.meta().mode, kind, None, false)
    {
        return refused(reason);
    }
    if let Some(err) = call.parse_err {
        return refused(format!("Invalid JSON arguments: {err}"));
    }

    // Plugin hooks fire for planner reads so a plugin auditing tool use sees
    // the whole turn; the user's `[hooks].pre_tool_use` shell hook does not,
    // because a plan pass would spawn a process per read to re-decide a
    // decision that cannot be a write.
    // The event is bound because the hook future borrows it, and a temporary
    // built inline would be dropped before the `select!` that awaits it.
    let hook_event = pacode_plugin::HookEvent::PreToolCall {
        name: call.name.clone(),
        input: call.input.clone(),
    };
    let hook = session.plugins.run_hooks(&hook_event);
    let input = tokio::select! {
        biased;
        _ = cancel.cancelled() => return cancelled(&call.call_id),
        outcome = hook => match outcome {
            Ok(pacode_plugin::HookResult::Continue) => call.input.clone(),
            Ok(pacode_plugin::HookResult::ModifyInput(input)) => input,
            Ok(pacode_plugin::HookResult::Deny { reason }) => {
                return refused(format!("Permission denied: {reason}"));
            }
            Err(error) => return refused(format!("plugin hook error: {error}")),
        },
    };

    let host = Arc::new(crate::host::SessionHost {
        session: session.clone(),
        agent: agent.id(),
    });
    let ctx = pacode_tools::ToolCtx {
        session: session.id.clone(),
        agent: agent.id(),
        agent_name: agent.info().name,
        call_id: call.call_id.clone(),
        cwd: session.meta().cwd.clone(),
        mode: session.meta().mode,
        host,
        cancel: cancel.clone(),
        output_cap_chars: session.config.context.tool_output_cap_chars,
        exec_yield_after: Duration::from_secs(session.config.exec.yield_after_secs),
        exec_default_timeout: Duration::from_secs(session.config.exec.default_timeout_secs),
        tool_name: Some(call.name.clone()),
        tool_kind: Some(kind),
        tools: registry.clone(),
    };

    let outcome = crate::turn::tools::call_bounded(&tool, input, &ctx, false).await;
    let output = match outcome {
        Ok(output) => output,
        Err(pacode_tools::ToolError::Cancelled) => return cancelled(&call.call_id),
        Err(pacode_tools::ToolError::Denied(reason)) => {
            return refused(format!("Permission denied: {reason}"));
        }
        Err(error) => return refused(error.to_string()),
    };

    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::PostToolCall {
            name: call.name.clone(),
            input: call.input.clone(),
            output: serde_json::json!({ "content": &output.content }),
        })
        .await;

    let content = cap_output_bytes(output.content, MAX_TOOL_OUTPUT_BYTES).into_owned();
    result(content, output.is_error)
}

fn cancelled(call_id: &CallId) -> (ContentBlock, bool) {
    (
        ContentBlock::ToolResult {
            call_id: call_id.clone(),
            content: "cancelled".to_string(),
            is_error: true,
        },
        true,
    )
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod planner_tests;
