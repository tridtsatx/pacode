//! Compaction (spec §6.4): one strategy, a four-stage pipeline that stops as soon as the
//! context is back under control.
//!
//! 1. **Shake** (`shake`): old, large tool results become stubs; the last
//!    `[context].keep_recent_turns` turns, the latest result per file path and pinned
//!    items are never touched.
//! 2. **Summary** (`prompt`, `pipeline`): the older part is folded into a structured,
//!    iterative LLM summary (an existing summary is UPDATED, never re-summarised).
//! 3. **Pinned data** (`pinned`): plan, background tasks, rules pointers and the
//!    [`CompactionPinned`] hook are appended verbatim after the summary.
//! 4. **Validation**: if the result is still above `compaction_target_ratio` of the
//!    window, K is halved and shake retried once (max 2 rounds). When the summary call
//!    fails the older span is replaced by a deterministic digest instead of an error.
//!
//! Defaults (`[context]`): `compaction_threshold = 0.85`, `compaction_target_ratio =
//! 0.40`, `keep_recent_turns = 6`, `keep_recent_messages = 6`. Each run is recorded in
//! the store's `compaction` table with its method (`shake` | `summary`) and the token
//! estimates before and after. Shaken stubs are in-memory only; persisted messages stay
//! unmodified, so a reloaded session is simply shaken again when it grows.

use std::sync::Arc;

use async_trait::async_trait;
use pacode_types::{CallId, Message, ModelRoute};

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

mod pinned;
mod pipeline;
mod prompt;
mod shake;

pub use pinned::{CompactionPinned, PINNED_MARKER, PinnedBlock, collect_pinned, split_pinned};
pub use pipeline::{
    CompactionMethod, MAX_ROUNDS, PipelineConfig, PipelineInput, PipelineOutput, Summarizer,
    build_summary_request as build_structured_summary_request, render_transcript,
};
pub use shake::{ShakeOutcome, ShakeParams, shake};

/// Default fallback context window when neither provider nor config specifies one.
pub const DEFAULT_FALLBACK_CONTEXT_WINDOW: u32 = 128_000;

/// Resolves the effective context window for a model per spec §6.4.
///
/// Resolution order:
/// 1. `model_info.context_window` if present and > 0 (advertised by provider).
/// 2. `config_default` if > 0 (from `[context].default_context_window`).
/// 3. [`DEFAULT_FALLBACK_CONTEXT_WINDOW`] (128,000 tokens).
pub fn resolve_context_window(
    model_info: Option<&pacode_types::ModelInfo>,
    config_default: u32,
) -> u32 {
    model_info
        .and_then(|info| info.context_window)
        .filter(|&w| w > 0)
        .unwrap_or(if config_default > 0 {
            config_default
        } else {
            DEFAULT_FALLBACK_CONTEXT_WINDOW
        })
}

/// True when the last reported input tokens (or the estimate) exceed
/// `threshold × context_window`.
pub fn needs_compaction(context_tokens: u32, context_window: u32, threshold: f32) -> bool {
    context_window > 0 && (context_tokens as f32) >= (context_window as f32) * threshold
}

/// Split history: everything except the last `keep_recent` messages (never splitting a
/// tool_use/tool_result pair) goes into the summary request.
///
/// A pure `keep_recent` tail is not always safe to fold away. On a long
/// session the last `keep_recent` messages can be a few tool calls, and the
/// user's actual request — "rename the config parser and update the callers" —
/// may be forty messages back behind them. A summary that drops it is worse than
/// no compaction: the agent resumes certain it was asked for something it was
/// never told. So the tail is scanned for messages that must survive on their
/// own terms (see [`move_cut_past_must_keep`]), and the cut moves *down* to
/// include them rather than folding them into a summary.
///
/// Both halves stay contiguous in history and the boundary is still a legal
/// tool_use/tool_result boundary, so the store's seq-based
/// `messages after upto_seq` reconstruction and provider-side pairing are
/// unaffected by how far down the cut moved.
pub fn split_for_compaction(
    messages: &[Arc<Message>],
    keep_recent: usize,
) -> (Vec<Arc<Message>>, Vec<Arc<Message>>) {
    let rows = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (index as u64, message.clone()))
        .collect::<Vec<_>>();
    let split = split_rows_for_compaction(&rows, keep_recent);
    (
        split
            .to_summarize
            .into_iter()
            .map(|(_, message)| message)
            .collect(),
        split
            .to_keep
            .into_iter()
            .map(|(_, message)| message)
            .collect(),
    )
}

type HistoryRows = Vec<(u64, Arc<Message>)>;

/// The result of deciding where to cut: the two halves of history, plus the
/// request the summary prompt must carry when the cut folded it away.
pub struct CompactionSplit {
    pub to_summarize: HistoryRows,
    pub to_keep: HistoryRows,
    pub active_request: Option<ActiveRequest>,
}

fn split_rows_for_compaction(rows: &[(u64, Arc<Message>)], keep_recent: usize) -> CompactionSplit {
    if rows.len() <= keep_recent {
        return CompactionSplit {
            to_summarize: Vec::new(),
            to_keep: rows.to_vec(),
            active_request: None,
        };
    }
    let requested_cut = rows.len().saturating_sub(keep_recent);
    let MoveCutOutcome {
        mut cut,
        active_request,
    } = move_cut_past_must_keep(rows, requested_cut);
    // Pull back to a legal boundary: the first kept message may not be a
    // `tool_result` whose `tool_use` was folded away, and an assistant message
    // carrying `tool_use` blocks may not be separated from the results that
    // answer them. Moving the cut down keeps the pair together at the cost of
    // keeping one more message than asked for.
    // `cut == rows.len()` (nothing kept) is already a legal boundary.
    while cut > 0 && cut < rows.len() {
        if rows[cut].1.role == pacode_types::Role::Tool
            || (rows[cut - 1].1.has_tool_uses()
                && cut < rows.len()
                && rows[cut].1.role == pacode_types::Role::Tool)
        {
            cut -= 1;
        } else {
            break;
        }
    }
    CompactionSplit {
        to_summarize: rows[..cut].to_vec(),
        to_keep: rows[cut..].to_vec(),
        active_request,
    }
}

/// The `tool_use` names whose results are worth keeping verbatim. A summary can
/// paraphrase "ran the tests" and lose which tests failed; the message carrying
/// the real output is the only copy there is.
///
/// A *failed* mutating call is included on the same reasoning as a successful
/// one — it is the record of a state the agent has to know it did not reach, and
/// the retry after it depends on that record.
const MUTATING_TOOLS: &[&str] = &[
    "edit",
    "write",
    "multi_edit",
    "bash",
    "bg",
    "cron",
    "monitor",
    "memory_write",
];

/// The index of the newest user message — the request currently being executed.
fn newest_user_index(rows: &[(u64, Arc<Message>)]) -> Option<usize> {
    rows.iter()
        .rposition(|(_, message)| message.role == pacode_types::Role::User)
}

/// The `call_id`s of every mutating `tool_use` in the history.
///
/// A `tool_result` carries only its `call_id`, so deciding whether *this* result
/// is worth keeping means knowing which tool answered it — and the answering
/// `tool_use` may sit many messages earlier. Hence one pass over the history up
/// front. It touches `ToolUse` headers only and never visits the result bodies,
/// so the pass is O(number of tool calls), not O(history bytes).
fn mutating_call_ids(rows: &[(u64, Arc<Message>)]) -> std::collections::HashSet<CallId> {
    let mut ids = std::collections::HashSet::new();
    for (_, message) in rows {
        for (id, name, _) in message.tool_uses() {
            if MUTATING_TOOLS.contains(&name) {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

/// How far `move_cut_past_must_keep` will reach below the requested cut to rescue
/// a *mutating tool result*. Twelve messages is roughly two or three tool-call
/// rounds: recent enough that the evidence is still what the agent is reasoning
/// about, bounded enough that a long session still compacts.
const MAX_EXTRA_KEPT_MESSAGES: usize = 12;

/// What the cut adjustment decided, and the request the summary must carry.
///
/// The split returns both because the two protection tiers are satisfied by
/// different mechanisms: the cut is what stops recent evidence from being
/// folded, while `active_request` is what keeps the executing request in play
/// once it is too far back to keep by moving a cut.
#[derive(Debug, Clone)]
struct MoveCutOutcome {
    cut: usize,
    active_request: Option<ActiveRequest>,
}

/// Move `cut` down so protected messages are not left in the folded prefix.
///
/// The cut is *always* bounded: both tiers are answered within
/// `MAX_EXTRA_KEPT_MESSAGES` of the requested cut, so the kept tail cannot grow
/// with the length of the session. A protected message further back than that is
/// folded like anything else, and the request — the one message whose loss
/// changes *what the agent is doing* rather than *what it knows* — is carried by
/// [`ActiveRequest`] instead, pinned into the summary prompt. Honouring it by
/// moving the cut instead would pin every message between it and the cut: a
/// request forty messages back would keep forty messages, and a session that
/// compacts repeatedly would re-grow without ever shrinking.
fn move_cut_past_must_keep(rows: &[(u64, Arc<Message>)], cut: usize) -> MoveCutOutcome {
    let mutating = mutating_call_ids(rows);
    let floor = cut.saturating_sub(MAX_EXTRA_KEPT_MESSAGES);
    let mut result = cut;

    for (index, (_, message)) in rows.iter().enumerate() {
        if index >= result || index < floor {
            continue;
        }
        if message.role == pacode_types::Role::Tool
            && message.content.iter().any(|block| match block {
                pacode_types::ContentBlock::ToolResult { call_id, .. } => {
                    mutating.contains(call_id)
                }
                _ => false,
            })
        {
            result = index;
            break;
        }
    }

    if result < cut {
        log::debug!(
            "compaction: keeping {} extra messages to preserve recent mutating results \
             (oldest kept index {result}, requested cut {cut}, lookback floor {floor})",
            cut - result
        );
    }
    MoveCutOutcome {
        cut: result,
        // Only the newest user turn qualifies — see `newest_user_index`.
        active_request: newest_user_index(rows)
            .filter(|index| *index < cut)
            .map(|index| ActiveRequest(rows[index].1.clone())),
    }
}

/// The request the agent is executing, recovered from the rows being folded.
///
/// This is the other half of the "never lose the request" guarantee. Holding the
/// executing request out of the fold would mean holding everything between it and
/// the cut out too; instead it is one short message handed to the summariser as a
/// term of the task — restate it or the summary is wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveRequest(Arc<Message>);

impl ActiveRequest {
    fn text(&self) -> String {
        self.0.text().to_string()
    }
}

/// The summarisation prompt: previous summary (if any) + the messages to fold in.
///
/// Returns shared `Arc` refs like `History::for_model` — the folded-in history
/// can be most of the conversation, cloning every block just to serialize it
/// once is pure churn. The slice goes through `sanitize_tool_pairing`: a prefix
/// cut can end on a `tool_use` whose result stays in `to_keep` (or the damage
/// was already persisted), and the summary request must not 400 for that.
///
/// The compaction pipeline itself sends the structured request
/// ([`build_structured_summary_request`]) and adds the active request with
/// [`active_request_note`].
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
    active_request: Option<&ActiveRequest>,
) -> Vec<Arc<Message>> {
    use pacode_types::{ContentBlock, Role};
    let (sanitized, _) = crate::agent::sanitize_tool_pairing(messages);
    let mut out = Vec::with_capacity(sanitized.len() + 3);
    out.push(Arc::new(Message::new(
        Role::System,
        vec![ContentBlock::Text {
            text: "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items. Be concise and structured.".to_string(),
        }],
    )));
    if let Some(prev) = previous_summary {
        out.push(Arc::new(Message::user(format!(
            "[Previous conversation summary to incorporate and update]\n{prev}"
        ))));
    }
    if let Some(request) = active_request {
        out.push(Arc::new(Message::user(active_request_note(request))));
    }
    out.extend(sanitized);
    out.push(Arc::new(Message::user(
        "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items.",
    )));
    out
}

/// The request in flight is a term of the task, not a fact to be summarised:
/// if it is folded away the agent resumes not knowing what it was asked to
/// do. Naming it explicitly is what makes that guarantee independent of how
/// far back the request sits — the alternative, keeping it out of the fold,
/// pins every message between it and the cut and stops compaction shrinking.
pub fn active_request_note(request: &ActiveRequest) -> String {
    format!(
        "[Request currently being executed — the agent must continue exactly this. Your \
         summary MUST open with it, in full and unparaphrased, before anything else. This is \
         a requirement of the summary, not a request to summarise.]\n{}",
        request.text()
    )
}

/// Which route summarizes the folded-in history, and whether a configured
/// `context.compaction_model` was dropped to keep the conversation inside the
/// ZDR boundary. The summary request carries the whole prefix of the
/// conversation, so under ZDR it may only go to a route flagged
/// `zdr_capable`; a non-capable configured model falls back to the agent's own
/// route, which the turn loop already validated. Returns the refusal reason
/// when not even the agent's route is usable.
fn pick_summary_route(
    configured: Option<ModelRoute>,
    agent_route: &ModelRoute,
    zdr_active: bool,
    zdr_capable: &dyn Fn(&str) -> bool,
) -> (ModelRoute, Option<String>) {
    let violation = |route: &ModelRoute| -> Option<String> {
        if !zdr_active || zdr_capable(&route.to_string()) {
            return None;
        }
        Some(format!(
            "{}: {route} is not flagged zdr_capable; the conversation summary would leave the \
             zero-data-retention boundary",
            crate::permissions::ZDR_BLOCKED
        ))
    };

    let Some(configured) = configured else {
        return (agent_route.clone(), violation(agent_route));
    };
    let Some(reason) = violation(&configured) else {
        return (configured, None);
    };
    if let Some(agent_reason) = violation(agent_route) {
        return (agent_route.clone(), Some(agent_reason));
    }
    log::warn!("{reason}; summarizing with {agent_route} instead of {configured}");
    (agent_route.clone(), None)
}

/// Summary model call backed by the provider registry.
struct ProviderSummarizer<'a> {
    session: &'a Session,
    route: ModelRoute,
    /// The executing request when the fold reaches it (see [`ActiveRequest`]):
    /// prepended to the summary request so the summary restates it.
    active_request: Option<ActiveRequest>,
}

#[async_trait]
impl Summarizer for ProviderSummarizer<'_> {
    async fn summarize(&self, mut request: Vec<Message>) -> Result<String, CoreError> {
        use futures::StreamExt;

        if let Some(active) = &self.active_request {
            let note = pacode_types::ContentBlock::Text {
                text: format!("{}\n\n", active_request_note(active)),
            };
            match request.first_mut() {
                Some(first) => first.content.insert(0, note),
                None => request.push(Message::user(active_request_note(active))),
            }
        }
        let provider = self.session.providers.resolve(&self.route).await?;
        let req = pacode_provider::CompletionRequest {
            model: self.route.model.clone(),
            system_static: prompt::SUMMARY_SYSTEM.to_string(),
            system_dynamic: String::new(),
            messages: request.into_iter().map(std::sync::Arc::new).collect(),
            tools: Vec::new(),
            effort: None,
            max_output_tokens: None,
        };
        let mut stream = provider.complete(req).await?;
        let mut text = String::new();
        while let Some(ev_res) = stream.next().await {
            match ev_res {
                Ok(pacode_types::StreamEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(
                    pacode_types::StreamEvent::MessageStart { .. }
                    | pacode_types::StreamEvent::ReasoningDelta { .. }
                    | pacode_types::StreamEvent::ReasoningSignature { .. }
                    | pacode_types::StreamEvent::ToolCallStart { .. }
                    | pacode_types::StreamEvent::ToolCallArgsDelta { .. }
                    | pacode_types::StreamEvent::Usage(_)
                    | pacode_types::StreamEvent::MessageEnd { .. },
                ) => {}
                Err(e) => return Err(CoreError::Provider(e)),
            }
        }
        Ok(text)
    }
}

/// Manual compaction (`/compact`): runs the pipeline without the threshold gates.
/// Returns false when there was nothing to do.
pub async fn compact(session: &Arc<Session>, agent: &Arc<Agent>) -> Result<bool, CoreError> {
    run_compaction(session, agent, None).await
}

/// Automatic compaction after a turn. `observed_tokens` is the last reported input size;
/// whatever it exceeds the history estimate by is treated as fixed overhead (system
/// prompt, tool definitions) when checking the threshold and target.
pub async fn compact_auto(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    observed_tokens: u32,
) -> Result<bool, CoreError> {
    run_compaction(session, agent, Some(observed_tokens)).await
}

fn history_tokens(summary: Option<&str>, messages: &[Arc<Message>]) -> u32 {
    messages.iter().map(|m| m.estimate_tokens()).fold(
        summary.map(pacode_types::estimate_tokens).unwrap_or(0),
        u32::saturating_add,
    )
}

async fn run_compaction(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    observed_tokens: Option<u32>,
) -> Result<bool, CoreError> {
    let ctx = &session.config.context;
    let agent_info = agent.info();

    let (rows, prev_summary) = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        (hist.rows(), hist.summary.clone())
    };
    let snapshot_len = rows.len();
    let messages: Vec<Arc<Message>> = rows.iter().map(|(_, message)| message.clone()).collect();

    let context_window = match session.providers.resolve(&agent_info.model).await {
        Ok(p) => resolve_context_window(
            Some(&p.model_info(&agent_info.model.model)),
            ctx.default_context_window,
        ),
        Err(_) => resolve_context_window(None, ctx.default_context_window),
    };
    let overhead_tokens = observed_tokens
        .map(|o| o.saturating_sub(history_tokens(prev_summary.as_deref(), &messages)))
        .unwrap_or(0);
    let (route, refusal) = pick_summary_route(
        ctx.compaction_model
            .as_deref()
            .and_then(|m| session.providers.parse_route(m)),
        &agent_info.model,
        session.zdr_active(),
        &|key| session.model_flags(key).zdr_capable,
    );
    if let Some(reason) = refusal {
        return Err(CoreError::Invalid(reason));
    }
    // The pipeline folds exactly the prefix `split_for_compaction` picks (shake never
    // changes the length), so the request it would fold away is known up front.
    let active_request = split_rows_for_compaction(&rows, ctx.keep_recent_messages).active_request;

    log::info!(
        "compaction starting: session={} agent={} messages={snapshot_len} forced={}",
        session.id,
        agent.id(),
        observed_tokens.is_none()
    );

    let cfg = PipelineConfig {
        context_window,
        threshold: ctx.compaction_threshold,
        target_ratio: ctx.compaction_target_ratio,
        keep_recent_turns: ctx.keep_recent_turns,
        keep_recent_messages: ctx.keep_recent_messages,
        overhead_tokens,
        force: observed_tokens.is_none(),
    };
    let input = PipelineInput {
        messages,
        previous_summary: prev_summary,
        pinned: collect_pinned(session, &agent.id()),
    };
    let summarizer = ProviderSummarizer {
        session: session.as_ref(),
        route,
        active_request,
    };
    let out = pipeline::run(input, &cfg, &summarizer).await;
    let Some(method) = out.method else {
        return Ok(false);
    };

    // Message seqs have gaps (cancelled admissions never land): the folded prefix ends
    // at the seq of its last row, and the kept rows keep their own seqs.
    let upto_seq = (out.folded > 0).then(|| rows[out.folded - 1].0);
    let rewritten: std::collections::HashMap<u64, Arc<Message>> = rows[out.folded..]
        .iter()
        .map(|(seq, _)| *seq)
        .zip(out.messages.iter().cloned())
        .collect();

    let _message_guard = agent.message_write_lock.clone().lock_owned().await;
    session
        .store
        .record_compaction(
            &session.id,
            &agent.id(),
            pacode_store::CompactionRecord {
                summary: upto_seq.and_then(|_| out.summary.clone()),
                upto_seq: upto_seq.unwrap_or(0),
                method: method.as_str().to_string(),
                tokens_before: out.tokens_before,
                tokens_after: out.tokens_after,
            },
        )
        .await?;

    {
        // Before the rewrite, so a checkpoint open across this line is already
        // marked lost by the time the settle asks: a word that went into the
        // summary is one the model may still have, and a word that fell off the
        // end is one it certainly did not.
        agent.note_compacted();
        let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        // Rows appended while the pipeline ran are kept as they are, after the
        // compacted ones; shaken rows replace their originals in place.
        let keep = hist
            .rows()
            .into_iter()
            .filter(|(seq, _)| upto_seq.is_none_or(|upto| *seq > upto))
            .map(|(seq, message)| {
                let message = rewritten.get(&seq).cloned().unwrap_or(message);
                (seq, message)
            })
            .collect::<Vec<_>>();
        if let Some(upto) = upto_seq {
            hist.summary = out.summary.clone();
            hist.summary_upto = hist.summary_upto.max(upto);
            hist.free_seqs.clear();
        }
        hist.messages = keep.iter().map(|(_, message)| message.clone()).collect();
        hist.message_seqs = keep.into_iter().map(|(seq, _)| seq).collect();
        hist.estimated_tokens = history_tokens(hist.summary.as_deref(), &hist.messages);
    }
    drop(_message_guard);

    log::info!(
        "compaction finished: session={} agent={} method={} tokens {} -> {} rounds={} stubbed={} folded={} summary_failed={}",
        session.id,
        agent.id(),
        method.as_str(),
        out.tokens_before,
        out.tokens_after,
        out.rounds,
        out.stubbed,
        out.folded,
        out.summary_failed
    );

    let mut text = format!(
        "Context compacted ({}): ~{} -> ~{} tokens",
        method.as_str(),
        out.tokens_before,
        out.tokens_after
    );
    if out.summary_failed {
        text.push_str("; summary model failed, used a deterministic digest");
    }
    session.emit_notice(agent, pacode_types::ToastLevel::Info, text);

    Ok(true)
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod compaction_tests;
