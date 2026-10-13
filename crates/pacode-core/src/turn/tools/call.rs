//! One tool call of a turn: budget, hooks, ZDR isolation, dispatch under the
//! turn's cancellation, and the result's transcript row and message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pacode_tools::ToolKind;
use pacode_tools::registry::{MAX_TOOL_OUTPUT_BYTES, cap_output_bytes};
use pacode_types::{
    AgentStatus, CallId, Event, Message, ToolStatus, TranscriptItem, TranscriptKind,
};
use tokio_util::sync::CancellationToken;

use super::budget::{BudgetLimits, TurnBudget};
use crate::agent::Agent;
use crate::session::Session;

pub(super) struct ToolCallInfo {
    pub(super) call_id: CallId,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
    pub(super) parse_err: Option<String>,
    pub(super) item_seq: u64,
    pub(super) title: String,
    pub(super) intent: Option<String>,
}

pub(super) struct ToolOutcome {
    pub(super) result: Message,
    pub(super) interrupted: bool,
}

pub(super) fn cancelled_outcome(
    session: &Arc<Session>,
    agent: &Agent,
    call: ToolCallInfo,
) -> ToolOutcome {
    let result = Message::tool_result(call.call_id.clone(), "cancelled", true);
    let item = TranscriptItem {
        seq: call.item_seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::ToolCall {
            call_id: call.call_id,
            name: call.name,
            title: call.title,
            intent: call.intent,
            status: ToolStatus::Error,
            preview: "cancelled".to_string(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));
    ToolOutcome {
        result,
        interrupted: true,
    }
}

/// `Denied: <first sentence of the reason>` — one bounded line for the cell
/// preview so a long policy/hook note cannot stretch the row.
fn denied_preview(reason: &str) -> String {
    let first_sentence = reason
        .lines()
        .next()
        .unwrap_or("")
        .split(". ")
        .next()
        .unwrap_or("")
        .trim();
    let short: String = first_sentence.chars().take(80).collect();
    format!("Denied: {short}")
}

/// Call `tool` under `ctx.cancel`, holding one of the agent's worker slots for
/// the whole call unless the tool takes its own (`holds_own_workers`). A turn
/// asking for a hundred reads must not open a hundred fds at once; the slot is
/// released as soon as the call returns or the turn is cancelled.
pub(crate) async fn call_bounded(
    tool: &Arc<dyn pacode_tools::Tool>,
    input: serde_json::Value,
    ctx: &pacode_tools::ToolCtx,
    holds_own_workers: bool,
) -> Result<pacode_tools::ToolOutput, pacode_tools::ToolError> {
    let _worker = if holds_own_workers {
        None
    } else {
        Some(
            ctx.tools
                .acquire_worker(&ctx.session, &ctx.agent, &ctx.cancel)
                .await?,
        )
    };
    let call_fut = tool.call(input, ctx);
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Err(pacode_tools::ToolError::Cancelled),
        res = call_fut => res,
    }
}

pub(super) async fn run_single_tool(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
    registry_snapshot: pacode_tools::ToolRegistry,
    budget: Arc<TurnBudget>,
    limits: BudgetLimits,
    mut call: ToolCallInfo,
) -> ToolOutcome {
    log::debug!(
        "tool call start: session={} agent={} tool={} call_id={}",
        session.id,
        agent.id(),
        call.name,
        call.call_id
    );
    agent.set_status(AgentStatus::RunningTool, Some(call.title.clone()));
    session.events.emit(Event::AgentUpdated(agent.info()));

    if cancel.is_cancelled() {
        return cancelled_outcome(&session, &agent, call);
    }

    let tool_opt = registry_snapshot.get(&call.name);
    let budget_denied = budget
        .reserve(
            limits,
            tool_opt.as_ref().map(|tool| tool.kind()) == Some(ToolKind::Exec),
        )
        .err();

    let mut hook_denied = None;
    if budget_denied.is_none() && call.parse_err.is_none() {
        let event = pacode_plugin::HookEvent::PreToolCall {
            name: call.name.clone(),
            input: call.input.clone(),
        };
        let hook = session.plugins.run_hooks(&event);
        let hook_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return cancelled_outcome(&session, &agent, call),
            result = hook => result,
        };
        match hook_result {
            Ok(pacode_plugin::HookResult::Deny { reason }) => {
                hook_denied = Some(reason);
            }
            Ok(pacode_plugin::HookResult::ModifyInput(new_input)) => {
                call.input = new_input;
            }
            Ok(pacode_plugin::HookResult::Continue) => {}
            Err(e) => {
                hook_denied = Some(format!("plugin hook error: {e}"));
            }
        }
    }

    // `[hooks].pre_tool_use` runs after plugin hooks so it sees the final
    // input; skipped when the call is already dead. Exit code 2 blocks the
    // call and the hook's stderr goes back to the model as the tool error.
    let mut user_hook_block = None;
    if budget_denied.is_none() && hook_denied.is_none() && call.parse_err.is_none() {
        let hook = crate::hooks::pre_tool_use(&session, &agent, &call.name, &call.input);
        let decision = tokio::select! {
            biased;
            _ = cancel.cancelled() => return cancelled_outcome(&session, &agent, call),
            decision = hook => decision,
        };
        match decision {
            crate::hooks::PreToolDecision::Allow => {}
            crate::hooks::PreToolDecision::Block(reason) => user_hook_block = Some(reason),
        }
    }

    // ZDR hard isolation: network tools (webfetch/websearch bypass the
    // permission gate) and every MCP call (`<server>__<tool>`, `__resource`)
    // are denied before dispatch. Checked here, not in `gate`, because MCP
    // tools reach the registry by name rather than by `ToolKind`.
    let zdr_block = if session.zdr_active()
        && (tool_opt.as_ref().map(|t| t.kind()) == Some(ToolKind::Network)
            || call.name.contains("__"))
    {
        Some(format!(
            "zdr_blocked: {} is disabled while zero-data-retention is active",
            call.name
        ))
    } else {
        None
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
        tool_kind: tool_opt.as_ref().map(|t| t.kind()),
        tools: registry_snapshot,
    };

    let start = Instant::now();
    let (content, is_error, preview, diff, task, status) = if let Some(reason) = &budget_denied {
        (
            reason.clone(),
            true,
            denied_preview(reason),
            None,
            None,
            ToolStatus::Error,
        )
    } else if let Some(reason) = hook_denied {
        (
            format!("Permission denied: {reason}"),
            true,
            denied_preview(&reason),
            None,
            None,
            ToolStatus::Denied,
        )
    } else if let Some(reason) = user_hook_block {
        (
            reason,
            true,
            "Blocked by hook".to_string(),
            None,
            None,
            ToolStatus::Denied,
        )
    } else if let Some(reason) = zdr_block {
        let preview = denied_preview(&reason);
        (reason, true, preview, None, None, ToolStatus::Denied)
    } else if let Some(err) = call.parse_err {
        (
            format!("Invalid JSON arguments: {err}"),
            true,
            "Invalid JSON".to_string(),
            None,
            None,
            ToolStatus::Error,
        )
    } else {
        match tool_opt {
            Some(tool) => {
                // `batch` takes one worker per op inside its own `run_op`, so
                // the outer call must not take one: it would hold a permit for
                // the whole batch while the batch's ops wait for permits.
                let holds_own_workers = call.name == pacode_tools::builtin::batch::NAME;
                let tool_res =
                    call_bounded(&tool, call.input.clone(), &ctx, holds_own_workers).await;
                match tool_res {
                    Ok(output) => {
                        // A task in the output runs in the background: started
                        // so, or handed over by the foreground wait, which is
                        // the one place that decides it (`wait_foreground`).
                        // The client still needs the row refreshed.
                        let status = if let Some(task_id) = output.task.as_ref() {
                            if let Some(tinfo) = session.tasks.info(task_id) {
                                session.events.emit(Event::TaskUpdated(tinfo));
                            }
                            ToolStatus::Backgrounded
                        } else if output.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        };
                        // A tool that did not set a preview still gets a cell
                        // body: the bounded content itself.
                        let preview = if output.preview.is_empty() {
                            pacode_types::truncate_head_tail(
                                &output.content,
                                pacode_types::PREVIEW_CAP_CHARS,
                            )
                        } else {
                            output.preview
                        };
                        (
                            output.content,
                            output.is_error,
                            preview,
                            output.diff,
                            output.task,
                            status,
                        )
                    }
                    Err(pacode_tools::ToolError::Cancelled) => {
                        return cancelled_outcome(&session, &agent, call);
                    }
                    Err(pacode_tools::ToolError::Denied(reason)) => (
                        format!("Permission denied: {reason}"),
                        true,
                        denied_preview(&reason),
                        None,
                        None,
                        ToolStatus::Denied,
                    ),
                    Err(err) => (
                        err.to_string(),
                        true,
                        err.to_string(),
                        None,
                        None,
                        ToolStatus::Error,
                    ),
                }
            }
            None => (
                format!("unknown tool: {}", call.name),
                true,
                "unknown tool".to_string(),
                None,
                None,
                ToolStatus::Error,
            ),
        }
    };

    // The hooks and the model get different copies of this result; see
    // `split_result_for_consumers` for why that split exists.
    let (content, output_val) = split_result_for_consumers(content, is_error);
    let preview = cap_output_bytes(preview, MAX_TOOL_OUTPUT_BYTES).into_owned();
    let duration_ms = start.elapsed().as_millis() as u64;
    log::debug!(
        "tool call result: session={} agent={} tool={} call_id={} status={:?} duration={}ms is_error={is_error}",
        session.id,
        agent.id(),
        call.name,
        call.call_id,
        status,
        duration_ms
    );

    if !cancel.is_cancelled() {
        let event = pacode_plugin::HookEvent::PostToolCall {
            name: call.name.clone(),
            input: call.input.clone(),
            output: output_val.clone(),
        };
        let hook = session.plugins.run_hooks(&event);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {},
            _ = hook => {},
        }
    }

    // `[hooks].post_tool_use` observes the outcome (denied and errored calls
    // included); it never blocks and hook stdout is ignored.
    if !cancel.is_cancelled() {
        let hook =
            crate::hooks::post_tool_use(&session, &agent, &call.name, &call.input, &output_val);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {},
            _ = hook => {},
        }
    }

    // Taken before the transcript item below moves the name out of `call`: the
    // result message knows the call id and not the tool, and the per-tool
    // ranking on `/analyze` is worthless without it.
    let tool_name = call.name.clone();
    let item = TranscriptItem {
        seq: call.item_seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::ToolCall {
            call_id: call.call_id.clone(),
            name: call.name,
            title: call.title,
            intent: call.intent,
            status,
            preview,
            diff,
            duration_ms: Some(duration_ms),
            task,
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));
    let result = Message::tool_result(call.call_id, content, is_error);
    // Charged to the tool that produced it, on the model-facing copy: the
    // ranking on `/analyze` is about what the model had to read back, and the
    // hook payload is a different text.
    session
        .accounting
        .record_tool_call(&tool_name, u64::from(result.estimate_tokens()));
    ToolOutcome {
        result,
        interrupted: cancel.is_cancelled(),
    }
}

/// Split one tool result between its two consumers, which have different needs.
///
/// Returns `(for_the_model, hook_payload)`.
///
/// **The model** is the consumer with a hard limit: it gets the capped copy and
/// `cap_output_bytes`' own marker, which is the only thing that can tell it the
/// result is a slice rather than a complete one.
///
/// **The hooks** are observers of what the tool actually did — a plugin writing
/// a transcript, a `[hooks].post_tool_use` matcher grepping for an error deep in
/// a 5 MiB log. Capping their copy at the model's byte cap made them blind to
/// exactly the oversized results they exist to catch. They get the uncut string,
/// plus `truncated_for_model_at_bytes` when the model's copy was cut, so a hook
/// that wants the truth has it and one that wants to reproduce the model's view
/// can.
///
/// The second copy is made only when there is something to copy: the oversized
/// branch is the one that has to keep two versions, and the under-cap branch
/// moves `content` into the payload and gives the model a borrow of it back.
pub(super) fn split_result_for_consumers(
    content: String,
    is_error: bool,
) -> (String, serde_json::Value) {
    if content.len() <= MAX_TOOL_OUTPUT_BYTES {
        let model_content = content.clone();
        return (
            model_content,
            serde_json::json!({ "content": content, "is_error": is_error }),
        );
    }
    let model_content = cap_output_bytes(content.clone(), MAX_TOOL_OUTPUT_BYTES).into_owned();
    let payload = serde_json::json!({
        "content": content,
        "is_error": is_error,
        "truncated_for_model_at_bytes": MAX_TOOL_OUTPUT_BYTES,
    });
    (model_content, payload)
}
