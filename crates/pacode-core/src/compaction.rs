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
use pacode_types::Message;

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
    build_summary_request, render_transcript,
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
pub fn split_for_compaction(
    messages: &[Arc<Message>],
    keep_recent: usize,
) -> (Vec<Arc<Message>>, Vec<Arc<Message>>) {
    if messages.len() <= keep_recent {
        return (Vec::new(), messages.to_vec());
    }
    let mut cut = messages.len().saturating_sub(keep_recent);
    // Never start the kept part with a tool result: move back to its tool_use message.
    while cut > 0 && cut < messages.len() && messages[cut].role == pacode_types::Role::Tool {
        cut -= 1;
    }
    (messages[..cut].to_vec(), messages[cut..].to_vec())
}

/// Summary model call backed by the provider registry.
struct ProviderSummarizer<'a> {
    session: &'a Session,
    route: pacode_types::ModelRoute,
}

#[async_trait]
impl Summarizer for ProviderSummarizer<'_> {
    async fn summarize(&self, request: Vec<Message>) -> Result<String, CoreError> {
        use futures::StreamExt;

        let provider = self.session.providers.resolve(&self.route).await?;
        let req = pacode_provider::CompletionRequest {
            model: self.route.model.clone(),
            system_static: prompt::SUMMARY_SYSTEM.to_string(),
            system_dynamic: String::new(),
            messages: request,
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

    let (messages, prev_summary, next_seq) = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        (hist.messages.clone(), hist.summary.clone(), hist.next_seq)
    };
    let snapshot_len = messages.len();
    let start_seq = next_seq.saturating_sub(snapshot_len as u64);

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
    let route = ctx
        .compaction_model
        .as_deref()
        .and_then(|m| session.providers.parse_route(m))
        .unwrap_or_else(|| agent_info.model.clone());

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
    };
    let out = pipeline::run(input, &cfg, &summarizer).await;
    let Some(method) = out.method else {
        return Ok(false);
    };

    let upto_seq = (out.folded > 0).then(|| start_seq + out.folded as u64 - 1);
    {
        let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        // Messages appended while the pipeline ran are kept after the compacted ones.
        let appended: Vec<Arc<Message>> =
            hist.messages.iter().skip(snapshot_len).cloned().collect();
        let mut new_messages = out.messages.clone();
        new_messages.extend(appended);
        if let Some(upto) = upto_seq {
            hist.summary = out.summary.clone();
            hist.summary_upto = upto;
        }
        hist.messages = new_messages;
        hist.estimated_tokens = history_tokens(hist.summary.as_deref(), &hist.messages);
    }

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
            text,
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
