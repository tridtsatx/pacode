//! Compaction (spec §6.4). Port the approach of jcode `jcode-compaction-core`.

use std::sync::Arc;

use pacode_types::Message;

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
    if messages.len() <= keep_recent {
        return (Vec::new(), messages.to_vec());
    }
    let mut cut = messages.len().saturating_sub(keep_recent);
    while cut > 0 {
        if messages[cut].role == pacode_types::Role::Tool
            || (messages[cut - 1].has_tool_uses()
                && cut < messages.len()
                && messages[cut].role == pacode_types::Role::Tool)
        {
            cut -= 1;
        } else {
            break;
        }
    }
    (messages[..cut].to_vec(), messages[cut..].to_vec())
}

/// The summarisation prompt: previous summary (if any) + the messages to fold in.
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
) -> Vec<Message> {
    use pacode_types::{ContentBlock, Role};
    let mut out = Vec::new();
    out.push(Message::new(
        Role::System,
        vec![ContentBlock::Text {
            text: "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items. Be concise and structured.".to_string(),
        }],
    ));
    if let Some(prev) = previous_summary {
        out.push(Message::user(format!(
            "[Previous conversation summary to incorporate and update]\n{prev}"
        )));
    }
    for msg in messages {
        out.push((**msg).clone());
    }
    out.push(Message::user(
        "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items.",
    ));
    out
}

/// Run the summary turn with `context.compaction_model` (or the agent's model), replace
/// the history, persist, emit a `Notice` item. Returns false when nothing was done.
pub async fn compact(session: &Arc<Session>, agent: &Arc<Agent>) -> Result<bool, CoreError> {
    use futures::StreamExt;

    let (to_summarize, to_keep, prev_summary, upto_seq) = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        let keep_recent = session.config.context.keep_recent_messages;
        let (to_summarize, to_keep) = split_for_compaction(&hist.messages, keep_recent);
        if to_summarize.is_empty() {
            return Ok(false);
        }
        let start_seq = hist.next_seq.saturating_sub(hist.messages.len() as u64);
        let upto_seq = start_seq + (to_summarize.len() as u64) - 1;
        (to_summarize, to_keep, hist.summary.clone(), upto_seq)
    };

    log::info!(
        "compaction starting: session={} agent={} upto_seq={upto_seq} messages_to_summarize={}",
        session.id,
        agent.id(),
        to_summarize.len()
    );

    let summary_messages = build_summary_request(prev_summary.as_deref(), &to_summarize);
    let agent_info = agent.info();
    let route = session
        .config
        .context
        .compaction_model
        .as_deref()
        .and_then(|m| session.providers.parse_route(m))
        .unwrap_or_else(|| agent_info.model.clone());

    let provider = session.providers.resolve(&route)?;
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
            Ok(_) => {}
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

    {
        let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        hist.summary = Some(summary.clone());
        hist.summary_upto = upto_seq;
        hist.messages = to_keep;
        let summary_tokens = pacode_types::estimate_tokens(&summary);
        let kept_tokens: u32 = hist.messages.iter().map(|m| m.estimate_tokens()).sum();
        hist.estimated_tokens = summary_tokens.saturating_add(kept_tokens);
    }

    session
        .store
        .save_compaction(&session.id, &agent.id(), &summary, upto_seq)
        .await?;

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
