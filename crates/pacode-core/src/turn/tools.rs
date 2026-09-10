//! Tool execution for the turn loop: concurrent/sequential grouping, permissions, cancellation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pacode_tools::ToolKind;
use pacode_types::{
    AgentStatus, CallId, Event, Message, ToolStatus, TranscriptItem, TranscriptKind,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;

struct ToolCallInfo {
    call_id: CallId,
    name: String,
    input: serde_json::Value,
    parse_err: Option<String>,
    item_seq: u64,
    title: String,
    intent: Option<String>,
}

enum ToolExecutionStatus {
    Ok,
    Cancelled,
}

impl ToolExecutionStatus {
    fn is_cancelled(&self) -> bool {
        matches!(self, ToolExecutionStatus::Cancelled)
    }
}

enum ToolChunk {
    Concurrent(Vec<ToolCallInfo>),
    Sequential(ToolCallInfo),
}

fn tool_title(name: &str, input: &serde_json::Value) -> String {
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            if k != "intent"
                && k != "accept_large_output"
                && let Some(s) = v.as_str()
            {
                let arg = s.trim();
                let truncated: String = arg.chars().take(60).collect();
                return format!("{name} {truncated}");
            }
        }
    }
    name.to_string()
}

async fn run_single_tool(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
    mut call: ToolCallInfo,
) -> ToolExecutionStatus {
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
        let stub = Message::tool_result(call.call_id.clone(), "cancelled", true);
        let (seq, arc_stub) = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(stub);
        let _ = session
            .store
            .append_message(&session.id, &agent.id(), seq, &arc_stub)
            .await;
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
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemUpdated(item));
        return ToolExecutionStatus::Cancelled;
    }

    let mut hook_denied = None;
    if call.parse_err.is_none() {
        match session
            .plugins
            .run_hooks(&pacode_plugin::HookEvent::PreToolCall {
                name: call.name.clone(),
                input: call.input.clone(),
            })
            .await
        {
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
    if hook_denied.is_none() && call.parse_err.is_none() {
        match crate::hooks::pre_tool_use(&session, &agent, &call.name, &call.input).await {
            crate::hooks::PreToolDecision::Allow => {}
            crate::hooks::PreToolDecision::Block(reason) => user_hook_block = Some(reason),
        }
    }

    let tool_opt = agent
        .tools
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(&call.name);

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
    };

    let start = Instant::now();
    let (content, is_error, preview, diff, task, status) = if let Some(reason) = hook_denied {
        (
            format!("Permission denied: {reason}"),
            true,
            "Denied".to_string(),
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
                let call_fut = tool.call(call.input.clone(), &ctx);
                let tool_res = tokio::select! {
                    _ = cancel.cancelled() => Err(pacode_tools::ToolError::Cancelled),
                    res = call_fut => res,
                };
                match tool_res {
                    Ok(output) => {
                        let status = if let Some(task_id) = output.task.as_ref() {
                            session.tasks.mark_backgrounded(task_id);
                            if let Some(tinfo) = session.tasks.info(task_id) {
                                session.events.emit(Event::TaskUpdated(tinfo));
                            }
                            ToolStatus::Backgrounded
                        } else if output.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        };
                        (
                            output.content,
                            output.is_error,
                            output.preview,
                            output.diff,
                            output.task,
                            status,
                        )
                    }
                    Err(pacode_tools::ToolError::Cancelled) => {
                        let stub = Message::tool_result(call.call_id.clone(), "cancelled", true);
                        let (seq, arc_stub) = agent
                            .history
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(stub);
                        let _ = session
                            .store
                            .append_message(&session.id, &agent.id(), seq, &arc_stub)
                            .await;
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
                            .unwrap_or_else(|p| p.into_inner())
                            .upsert(item.clone());
                        session.events.emit(Event::ItemUpdated(item));
                        return ToolExecutionStatus::Cancelled;
                    }
                    Err(pacode_tools::ToolError::Denied(reason)) => (
                        format!("Permission denied: {reason}"),
                        true,
                        "Denied".to_string(),
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

    let output_val = serde_json::json!({
        "content": content,
        "is_error": is_error,
    });
    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::PostToolCall {
            name: call.name.clone(),
            input: call.input.clone(),
            output: output_val.clone(),
        })
        .await;

    // `[hooks].post_tool_use` observes the outcome (denied and errored calls
    // included); it never blocks and hook stdout is ignored.
    crate::hooks::post_tool_use(&session, &agent, &call.name, &call.input, &output_val).await;

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
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));

    let res_msg = Message::tool_result(call.call_id, content, is_error);
    let (seq, arc_msg) = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(res_msg);
    let _ = session
        .store
        .append_message(&session.id, &agent.id(), seq, &arc_msg)
        .await;

    ToolExecutionStatus::Ok
}

pub async fn execute_tool_calls(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    tool_calls: Vec<(CallId, String, serde_json::Value, Option<String>)>,
) -> bool {
    let mut prepared_calls = Vec::new();
    for (call_id, name, input, parse_err) in tool_calls {
        let title = tool_title(&name, &input);
        let intent = pacode_tools::intent_of(&input);
        let item_seq = agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = TranscriptItem {
            seq: item_seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                title: title.clone(),
                intent: intent.clone(),
                status: ToolStatus::Running,
                preview: String::new(),
                diff: None,
                duration_ms: None,
                task: None,
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemAdded(item));

        prepared_calls.push(ToolCallInfo {
            call_id,
            name,
            input,
            parse_err,
            item_seq,
            title,
            intent,
        });
    }

    let mut chunks: Vec<ToolChunk> = Vec::new();
    for call in prepared_calls {
        let is_concurrent = agent
            .tools
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&call.name)
            .map(|t| matches!(t.kind(), ToolKind::ReadOnly | ToolKind::Network))
            .unwrap_or(false);

        if is_concurrent {
            match chunks.last_mut() {
                Some(ToolChunk::Concurrent(list)) => list.push(call),
                _ => chunks.push(ToolChunk::Concurrent(vec![call])),
            }
        } else {
            chunks.push(ToolChunk::Sequential(call));
        }
    }

    let mut interrupted = false;
    let mut remaining_calls_to_cancel = Vec::new();

    for chunk in chunks {
        if cancel.is_cancelled() {
            interrupted = true;
            match chunk {
                ToolChunk::Concurrent(list) => remaining_calls_to_cancel.extend(list),
                ToolChunk::Sequential(call) => remaining_calls_to_cancel.push(call),
            }
            continue;
        }

        match chunk {
            ToolChunk::Concurrent(list) => {
                let futures = list.into_iter().map(|call| {
                    let session = session.clone();
                    let agent = agent.clone();
                    let cancel = cancel.clone();
                    async move { run_single_tool(session, agent, cancel, call).await }
                });
                let results = futures::future::join_all(futures).await;
                for res in results {
                    if res.is_cancelled() {
                        interrupted = true;
                    }
                }
            }
            ToolChunk::Sequential(call) => {
                let res =
                    run_single_tool(session.clone(), agent.clone(), cancel.clone(), call).await;
                if res.is_cancelled() {
                    interrupted = true;
                }
            }
        }
    }

    if interrupted {
        for call in remaining_calls_to_cancel {
            let stub = Message::tool_result(call.call_id.clone(), "cancelled", true);
            let (seq, arc_stub) = agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(stub);
            let _ = session
                .store
                .append_message(&session.id, &agent.id(), seq, &arc_stub)
                .await;
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
                .unwrap_or_else(|p| p.into_inner())
                .upsert(item.clone());
            session.events.emit(Event::ItemUpdated(item));
        }
    }

    interrupted
}
