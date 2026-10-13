//! Tool execution for the turn loop: concurrent/sequential grouping, permissions, cancellation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use pacode_tools::ToolKind;
use pacode_tools::registry::{MAX_TOOL_OUTPUT_BYTES, cap_output_bytes};
use pacode_types::{
    AgentId, AgentStatus, CallId, ContentBlock, Event, Message, Role, SessionId, ToolStatus,
    TranscriptItem, TranscriptKind, TurnId,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;
use crate::turn::stream::PendingToolCall;

/// Turns whose budget counters are still held, before the oldest is dropped.
/// Dropping an entry only costs that turn its counters (it restarts from zero),
/// never its lock — the map is a ledger, not a guard.
const MAX_TRACKED_TURN_BUDGETS: usize = 1024;

/// Per-turn allowances, and the two knobs they were read from. `[limits]
/// .max_tool_calls_per_turn` / `max_exec_calls_per_turn` carried these until the
/// limits table was cut down to the provider ceiling; the values below are the
/// ones those keys defaulted to, and `turn_budget_limits` is the single place to
/// read them again once the keys are back.
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: u32 = 1024;
const DEFAULT_MAX_EXEC_CALLS_PER_TURN: u32 = 128;

fn turn_budget_limits(_session: &Session) -> BudgetLimits {
    BudgetLimits {
        tool_calls: DEFAULT_MAX_TOOL_CALLS_PER_TURN,
        exec_calls: DEFAULT_MAX_EXEC_CALLS_PER_TURN,
    }
}

#[derive(Default)]
struct TurnBudget {
    tool_calls: AtomicU32,
    exec_calls: AtomicU32,
}

#[derive(Clone, Copy)]
struct BudgetLimits {
    tool_calls: u32,
    exec_calls: u32,
}

/// A turn's spend against `[limits]`. The counters live for the turn: a new turn
/// of the same agent gets a fresh pair, and the old one is dropped the moment
/// that agent dispatches again.
struct BudgetedTurn {
    budget: Arc<TurnBudget>,
    created: u64,
}

type TurnBudgetKey = (SessionId, AgentId, Option<TurnId>);

static TURN_BUDGETS: LazyLock<Mutex<HashMap<TurnBudgetKey, BudgetedTurn>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TURN_BUDGET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TurnBudget {
    /// Charge one model-requested call to this turn. Every call is charged, the
    /// refused ones included, so a model cannot spend the budget by retrying a
    /// call that was cut off. `0` in a limit means no limit.
    fn reserve(&self, limits: BudgetLimits, is_exec: bool) -> Result<(), String> {
        if limits.tool_calls > 0 {
            let used = self.tool_calls.fetch_add(1, Ordering::Relaxed) + 1;
            if used > limits.tool_calls {
                return Err(format!(
                    "per-turn tool call budget of {} is spent; this call was refused \
                     automatically, not refused by the user — do not retry it; finish the turn \
                     and report what you have so far",
                    limits.tool_calls
                ));
            }
        }
        if is_exec && limits.exec_calls > 0 {
            let used = self.exec_calls.fetch_add(1, Ordering::Relaxed) + 1;
            if used > limits.exec_calls {
                return Err(format!(
                    "per-turn exec budget of {} is spent; this call was refused automatically, not \
                     refused by the user — do not retry it; finish the turn and report what you \
                     have so far",
                    limits.exec_calls
                ));
            }
        }
        Ok(())
    }
}

/// The spending counters of the turn this dispatch belongs to. The agent's
/// current turn id is the epoch: a fresh id means a fresh budget, and a
/// dispatch outside a turn (only tests do that) gets one of its own.
fn turn_budget(session: &Session, agent: &Agent) -> Arc<TurnBudget> {
    let key = (session.id.clone(), agent.id(), agent.current_turn());
    let mut budgets = TURN_BUDGETS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    budgets.retain(|held, _| held.0 != key.0 || held.1 != key.1 || held.2 == key.2);
    if let Some(held) = budgets.get(&key) {
        return Arc::clone(&held.budget);
    }
    let budget = Arc::new(TurnBudget::default());
    budgets.insert(
        key,
        BudgetedTurn {
            budget: Arc::clone(&budget),
            created: TURN_BUDGET_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        },
    );
    while budgets.len() > MAX_TRACKED_TURN_BUDGETS {
        let Some(oldest) = budgets
            .iter()
            .min_by_key(|(_, held)| held.created)
            .map(|(held, _)| held.clone())
        else {
            break;
        };
        budgets.remove(&oldest);
    }
    budget
}

struct ToolCallInfo {
    call_id: CallId,
    name: String,
    input: serde_json::Value,
    parse_err: Option<String>,
    item_seq: u64,
    title: String,
    intent: Option<String>,
}

struct ToolOutcome {
    result: Message,
    interrupted: bool,
}

fn cancelled_outcome(session: &Arc<Session>, agent: &Agent, call: ToolCallInfo) -> ToolOutcome {
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

async fn run_single_tool(
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
fn split_result_for_consumers(content: String, is_error: bool) -> (String, serde_json::Value) {
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
