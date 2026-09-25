//! Compaction (spec §6.4). Port the approach of jcode `jcode-compaction-core`.

use std::sync::Arc;

use pacode_types::{Message, ModelRoute};

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

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
pub fn split_for_compaction(
    messages: &[Arc<Message>],
    keep_recent: usize,
) -> (Vec<Arc<Message>>, Vec<Arc<Message>>) {
    let rows = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (index as u64, message.clone()))
        .collect::<Vec<_>>();
    let (summary, keep) = split_rows_for_compaction(&rows, keep_recent);
    (
        summary.into_iter().map(|(_, message)| message).collect(),
        keep.into_iter().map(|(_, message)| message).collect(),
    )
}

type HistoryRows = Vec<(u64, Arc<Message>)>;

fn split_rows_for_compaction(
    rows: &[(u64, Arc<Message>)],
    keep_recent: usize,
) -> (HistoryRows, HistoryRows) {
    if rows.len() <= keep_recent {
        return (Vec::new(), rows.to_vec());
    }
    let mut cut = rows.len().saturating_sub(keep_recent);
    while cut > 0 {
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
    (rows[..cut].to_vec(), rows[cut..].to_vec())
}

/// The summarisation prompt: previous summary (if any) + the messages to fold in.
///
/// Returns shared `Arc` refs like `History::for_model` — the folded-in history
/// can be most of the conversation, cloning every block just to serialize it
/// once is pure churn. The slice goes through `sanitize_tool_pairing`: a prefix
/// cut can end on a `tool_use` whose result stays in `to_keep` (or the damage
/// was already persisted), and the summary request must not 400 for that.
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
) -> Vec<Arc<Message>> {
    use pacode_types::{ContentBlock, Role};
    let (sanitized, _) = crate::agent::sanitize_tool_pairing(messages);
    let mut out = Vec::with_capacity(sanitized.len() + 2);
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
    out.extend(sanitized);
    out.push(Arc::new(Message::user(
        "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items.",
    )));
    out
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

/// Run the summary turn with `context.compaction_model` (or the agent's model), replace
/// the history, persist, emit a `Notice` item. Returns false when nothing was done.
pub async fn compact(session: &Arc<Session>, agent: &Arc<Agent>) -> Result<bool, CoreError> {
    use futures::StreamExt;

    let (to_summarize, prev_summary, upto_seq) = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        let keep_recent = session.config.context.keep_recent_messages;
        let (to_summarize, _) = split_rows_for_compaction(&hist.rows(), keep_recent);
        if to_summarize.is_empty() {
            return Ok(false);
        }
        let upto_seq = to_summarize
            .last()
            .map(|(seq, _)| *seq)
            .expect("non-empty compaction prefix");
        (
            to_summarize
                .into_iter()
                .map(|(_, message)| message)
                .collect::<Vec<_>>(),
            hist.summary.clone(),
            upto_seq,
        )
    };

    log::info!(
        "compaction starting: session={} agent={} upto_seq={upto_seq} messages_to_summarize={}",
        session.id,
        agent.id(),
        to_summarize.len()
    );

    let agent_info = agent.info();
    let (route, refusal) = pick_summary_route(
        session
            .config
            .context
            .compaction_model
            .as_deref()
            .and_then(|m| session.providers.parse_route(m)),
        &agent_info.model,
        session.zdr_active(),
        &|key| session.model_flags(key).zdr_capable,
    );
    if let Some(reason) = refusal {
        return Err(CoreError::Invalid(reason));
    }

    let summary_messages = build_summary_request(prev_summary.as_deref(), &to_summarize);

    let provider = session.providers.resolve(&route).await?;
    let req = pacode_provider::CompletionRequest {
        model: route.model.clone(),
        system_static: "You are a concise conversation summarizer.".to_string(),
        system_dynamic: String::new(),
        messages: summary_messages,
        tools: Vec::new(),
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await?;
    let mut summary_text = String::new();
    while let Some(ev_res) = stream.next().await {
        match ev_res {
            Ok(pacode_types::StreamEvent::TextDelta { text }) => {
                summary_text.push_str(&text);
            }
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

    let summary = summary_text.trim().to_string();
    if summary.is_empty() {
        return Ok(false);
    }

    log::info!(
        "compaction finished: session={} agent={} upto_seq={upto_seq} summary_len={}",
        session.id,
        agent.id(),
        summary.len()
    );

    let _message_guard = agent.message_write_lock.clone().lock_owned().await;
    session
        .store
        .save_compaction(&session.id, &agent.id(), &summary, upto_seq)
        .await?;

    {
        let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        let keep = hist
            .rows()
            .into_iter()
            .filter(|(seq, _)| *seq > upto_seq)
            .collect::<Vec<_>>();
        hist.summary = Some(summary.clone());
        hist.summary_upto = hist.summary_upto.max(upto_seq);
        hist.messages = keep.iter().map(|(_, message)| message.clone()).collect();
        hist.message_seqs = keep.into_iter().map(|(seq, _)| seq).collect();
        hist.free_seqs.clear();
        let summary_tokens = pacode_types::estimate_tokens(&summary);
        let kept_tokens: u32 = hist.messages.iter().map(|m| m.estimate_tokens()).sum();
        hist.estimated_tokens = summary_tokens.saturating_add(kept_tokens);
    }
    drop(_message_guard);

    let notice_item = pacode_types::TranscriptItem {
        seq: agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq(),
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: pacode_types::TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Info,
            text: format!("Conversation history compacted up to seq {upto_seq}"),
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(notice_item.clone());
    session
        .events
        .emit(pacode_types::Event::ItemAdded(notice_item));

    Ok(true)
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod compaction_tests;
