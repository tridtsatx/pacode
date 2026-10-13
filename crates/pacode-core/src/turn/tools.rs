//! Tool execution for the turn loop: concurrent/sequential grouping, permissions, cancellation.

use std::sync::Arc;

use pacode_types::{
    CallId, ContentBlock, Event, Message, Role, ToolStatus, TranscriptItem, TranscriptKind,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;
use crate::turn::stream::PendingToolCall;

mod budget;
mod call;

#[cfg(test)]
use budget::{BudgetLimits, TurnBudget};
use budget::{turn_budget, turn_budget_limits};
pub(crate) use call::call_bounded;
#[cfg(test)]
use call::split_result_for_consumers;
use call::{ToolCallInfo, ToolOutcome, cancelled_outcome, run_single_tool};

async fn commit_tool_outcome(session: &Arc<Session>, agent: &Arc<Agent>, outcome: ToolOutcome) {
    agent.commit_pending_user_messages();
    let (seq, message) = agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(outcome.result);
    let _ = session
        .append_agent_message(agent.as_ref(), seq, &message)
        .await;
}

enum ToolChunk {
    Concurrent(Vec<ToolCallInfo>),
    Sequential(ToolCallInfo),
}

/// Input keys that name what the tool acts on, in priority order — `path`
/// beats `content` for `write`, `pattern` beats `path` for `grep`.
pub(crate) const TITLE_KEYS: &[&str] = &[
    "command",
    "pattern",
    "query",
    "path",
    "file_path",
    "file",
    "url",
    "uri",
];

fn tool_title(name: &str, input: &serde_json::Value) -> String {
    crate::transcript::tool_title(name, input, TITLE_KEYS)
}

/// The preview the streamed cell already shows (raw args so far), kept across
/// the dispatch refresh so the row does not blink empty before the tool lands.
/// Empty when the cell is missing or belongs to a different seq.
fn streamed_args_preview(agent: &Agent, call_id: &CallId, seq: u64) -> String {
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .tool_item(call_id)
        .and_then(|item| match &item.kind {
            TranscriptKind::ToolCall { preview, .. } if item.seq == seq => Some(preview.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub async fn execute_tool_calls(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    tool_calls: Vec<PendingToolCall>,
) -> bool {
    let registry_snapshot = agent
        .tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .snapshot();
    let budget = turn_budget(session, agent);
    let limits = turn_budget_limits(session);
    let mut prepared_calls = Vec::new();
    for call in tool_calls {
        let title = tool_title(&call.name, &call.input);
        let intent = pacode_tools::intent_of(&call.input);
        // Calls the stream announced at `ToolCallStart` already have a cell
        // (its preview shows the raw args): refresh it in place — real
        // title/intent now, the args preview stays until the tool output
        // replaces it. A call that arrived without a start event gets a fresh
        // cell. On a JSON parse error the existing cell is updated to
        // "Invalid JSON" by `run_single_tool` — no second cell appears.
        let (item_seq, preview, streamed) = match call.item_seq {
            Some(seq) => (seq, streamed_args_preview(agent, &call.call_id, seq), true),
            None => (
                agent
                    .transcript
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .next_seq(),
                String::new(),
                false,
            ),
        };
        let item = TranscriptItem {
            seq: item_seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::ToolCall {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                title: title.clone(),
                intent: intent.clone(),
                status: ToolStatus::Running,
                preview,
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
        if streamed {
            session.events.emit(Event::ItemUpdated(item));
        } else {
            session.events.emit(Event::ItemAdded(item));
        }

        prepared_calls.push(ToolCallInfo {
            call_id: call.call_id,
            name: call.name,
            input: call.input,
            parse_err: call.parse_err,
            item_seq,
            title,
            intent,
        });
    }

    // Decided on the model's input. A plugin hook may still rewrite a call
    // inside `run_single_tool`; the tool then checks and asks for the input it
    // actually runs, so the permission stays exact, only the call keeps the
    // group it was placed in.
    let cwd = session.meta().cwd;
    let mut chunks: Vec<ToolChunk> = Vec::new();
    for call in prepared_calls {
        let is_concurrent = registry_snapshot
            .get(&call.name)
            .is_some_and(|tool| tool.runs_concurrently(&call.input, &cwd));

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
    for chunk in chunks {
        if cancel.is_cancelled() {
            interrupted = true;
            match chunk {
                ToolChunk::Concurrent(list) => {
                    for call in list {
                        commit_tool_outcome(
                            session,
                            agent,
                            cancelled_outcome(session, agent, call),
                        )
                        .await;
                    }
                }
                ToolChunk::Sequential(call) => {
                    commit_tool_outcome(session, agent, cancelled_outcome(session, agent, call))
                        .await;
                }
            }
            continue;
        }

        match chunk {
            ToolChunk::Concurrent(list) => {
                let futures = list.into_iter().map(|call| {
                    let session = session.clone();
                    let agent = agent.clone();
                    let cancel = cancel.clone();
                    let registry = registry_snapshot.clone();
                    let budget = Arc::clone(&budget);
                    async move {
                        run_single_tool(session, agent, cancel, registry, budget, limits, call)
                            .await
                    }
                });
                for outcome in futures::future::join_all(futures).await {
                    interrupted |= outcome.interrupted;
                    commit_tool_outcome(session, agent, outcome).await;
                }
            }
            ToolChunk::Sequential(call) => {
                let outcome = run_single_tool(
                    session.clone(),
                    agent.clone(),
                    cancel.clone(),
                    registry_snapshot.clone(),
                    Arc::clone(&budget),
                    limits,
                    call,
                )
                .await;
                interrupted |= outcome.interrupted;
                commit_tool_outcome(session, agent, outcome).await;
            }
        }
    }

    interrupted
}

/// Append+persist a synthetic error `tool_result` for every `tool_use` in the
/// trailing assistant message that never got one.
///
/// `run_single_tool` and the interrupted-chunk loop stub the calls they can
/// reach; what they cannot cover is the turn task dying mid-dispatch — a tool
/// panic unwinds `execute_tool_calls` before results land, and an abort drops
/// it outright — leaving persisted `tool_use`s unanswered so every later
/// request 400s (`tool_use ids found without tool_result`). Runs once per turn
/// end from the spawn supervisor; an intact tail makes it a no-op.
///
/// Only the tail is repaired: a `tool_use` stranded deeper in history (e.g.
/// behind a later user message, or loaded corrupted) is healed wire-side by
/// `History::for_model`'s pairing pass instead — history is append-only and
/// the store is too.
pub(crate) async fn stub_dangling_tool_calls(session: &Arc<Session>, agent: &Arc<Agent>) {
    // (call_id, name) of the trailing assistant's still-unanswered calls.
    let missing: Vec<(CallId, String)> = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        let mut idx = hist.messages.len();
        let mut answered: Vec<CallId> = Vec::new();
        while idx > 0 && hist.messages[idx - 1].role == Role::Tool {
            for block in &hist.messages[idx - 1].content {
                if let ContentBlock::ToolResult { call_id, .. } = block {
                    answered.push(call_id.clone());
                }
            }
            idx -= 1;
        }
        let Some(last) = idx.checked_sub(1).map(|i| &hist.messages[i]) else {
            return;
        };
        if last.role != Role::Assistant {
            return;
        }
        last.tool_uses()
            .filter(|(id, _, _)| !answered.contains(id))
            .map(|(id, name, _)| (id.clone(), name.to_string()))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    log::warn!(
        "turn ended with {} unanswered tool call(s), writing stubs: session={} agent={}",
        missing.len(),
        session.id,
        agent.id()
    );
    for (call_id, name) in missing {
        let stub =
            Message::tool_result(call_id.clone(), crate::agent::INTERRUPTED_TOOL_RESULT, true);
        let (seq, arc_stub) = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(stub);
        let _ = session
            .append_agent_message(agent.as_ref(), seq, &arc_stub)
            .await;
        // The cell allocated at `ToolCallStart` must not stay `Running`.
        let prev = agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .tool_item(&call_id)
            .cloned();
        if let Some(prev) = prev
            && let TranscriptKind::ToolCall {
                title,
                intent,
                duration_ms,
                task,
                ..
            } = &prev.kind
        {
            let item = TranscriptItem {
                seq: prev.seq,
                agent: agent.id(),
                ts_ms: pacode_types::now_ms(),
                kind: TranscriptKind::ToolCall {
                    call_id: call_id.clone(),
                    name,
                    title: title.clone(),
                    intent: intent.clone(),
                    status: ToolStatus::Error,
                    preview: crate::agent::INTERRUPTED_TOOL_RESULT.to_string(),
                    diff: None,
                    duration_ms: *duration_ms,
                    task: task.clone(),
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
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tools_tests;

#[cfg(test)]
#[path = "tools_bash_tests.rs"]
mod tools_bash_tests;
