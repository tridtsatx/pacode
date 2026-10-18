//! The compaction pipeline over an in-memory message list (no session, no I/O besides
//! the injected [`Summarizer`]), so every stage is unit-testable.
//!
//! Order: 1) mechanical shake, 2) LLM summary of the older part (iterative), 3) pinned
//! data re-injected after the summary, 4) validation against the target with at most
//! one extra shake round (`MAX_ROUNDS` = 2). A failing summary call degrades to shake
//! plus a deterministic digest instead of an error.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use pacode_types::{ContentBlock, Message, Role, estimate_tokens, truncate_head_tail};

use super::pinned::{PinnedBlock, attach_pinned, digest, split_pinned};
use super::prompt;
use super::shake::{ShakeParams, shake};
use super::split_for_compaction;
use crate::CoreError;

/// Hard cap on shake/summary rounds per compaction.
pub const MAX_ROUNDS: u8 = 2;
/// Tool results smaller than this are never shaken.
pub const SHAKE_MIN_CHARS: usize = 1_000;
/// Per tool result cap when rendering the span for the summary model.
const RENDER_RESULT_CAP: usize = 2_000;
/// Per tool call argument cap when rendering the span.
const RENDER_ARGS_CAP: usize = 600;
/// Cap for the fallback (digest) summary body.
const FALLBACK_BODY_CAP: usize = 16_000;

/// Which stage produced the result; persisted in the `compaction` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionMethod {
    /// Only mechanical stubbing (or the deterministic digest fallback).
    Shake,
    /// An LLM summary was produced.
    Summary,
}

impl CompactionMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionMethod::Shake => "shake",
            CompactionMethod::Summary => "summary",
        }
    }
}

/// Produces summary text from a prepared request (the provider call in production).
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, request: Vec<Message>) -> Result<String, CoreError>;
}

#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub context_window: u32,
    /// Fraction of the window at which compaction triggers.
    pub threshold: f32,
    /// Fraction of the window compaction aims for.
    pub target_ratio: f32,
    pub keep_recent_turns: usize,
    pub keep_recent_messages: usize,
    /// Tokens outside history and summary (system prompt, tool definitions).
    pub overhead_tokens: u32,
    /// Manual compaction: skip the threshold gates and always summarise if possible.
    pub force: bool,
}

impl PipelineConfig {
    fn threshold_tokens(&self) -> u32 {
        (self.context_window as f32 * self.threshold) as u32
    }

    fn target_tokens(&self) -> u32 {
        (self.context_window as f32 * self.target_ratio) as u32
    }
}

pub struct PipelineInput {
    pub messages: Vec<Arc<Message>>,
    /// The stored summary (body plus pinned section), if any.
    pub previous_summary: Option<String>,
    pub pinned: Vec<PinnedBlock>,
}

#[derive(Clone, Debug)]
pub struct PipelineOutput {
    pub messages: Vec<Arc<Message>>,
    /// Summary (body plus pinned section) valid for `messages`.
    pub summary: Option<String>,
    /// Number of leading input messages folded into the summary.
    pub folded: usize,
    /// `None` when nothing changed.
    pub method: Option<CompactionMethod>,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub rounds: u8,
    pub stubbed: usize,
    /// The summary call failed or returned nothing; the digest was used instead.
    pub summary_failed: bool,
}

fn messages_tokens(messages: &[Arc<Message>]) -> u32 {
    messages
        .iter()
        .map(|m| m.estimate_tokens())
        .fold(0, u32::saturating_add)
}

fn summary_tokens(summary: &Option<String>) -> u32 {
    summary.as_deref().map(estimate_tokens).unwrap_or(0)
}

/// Render a span as plain text for the summary model. Tool results are capped; the
/// span is text, so no tool definitions are needed on the summary request.
pub fn render_transcript(messages: &[Arc<Message>]) -> String {
    let mut out = String::new();
    for msg in messages {
        let label = match msg.role {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
        };
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    let _ = writeln!(out, "[{label}]\n{text}\n");
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let args = truncate_head_tail(&input.to_string(), RENDER_ARGS_CAP);
                    let _ = writeln!(out, "[Tool call] {name} {args}\n");
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let tag = if *is_error {
                        "Tool result (error)"
                    } else {
                        "Tool result"
                    };
                    let body = truncate_head_tail(content, RENDER_RESULT_CAP);
                    let _ = writeln!(out, "[{tag}]\n{body}\n");
                }
                ContentBlock::Reasoning { .. } => {}
                ContentBlock::Image {
                    mime,
                    width,
                    height,
                    source_path,
                    ..
                } => {
                    let dims = match (width, height) {
                        (Some(w), Some(h)) => format!(" {w}x{h}"),
                        _ => String::new(),
                    };
                    let from = source_path
                        .as_deref()
                        .map(|p| format!(" from {p}"))
                        .unwrap_or_default();
                    let _ = writeln!(out, "[{label}] [image {mime}{dims}{from}]\n");
                }
            }
        }
    }
    out
}

/// The summary request: previous summary body (if any) + the span to fold in, followed
/// by the structured instructions. A previous summary switches to the UPDATE prompt.
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
) -> Vec<Message> {
    let mut text = String::new();
    if let Some(prev) = previous_summary {
        let _ = writeln!(text, "<previous-summary>\n{prev}\n</previous-summary>\n");
    }
    let _ = writeln!(
        text,
        "<conversation>\n{}</conversation>\n",
        render_transcript(messages)
    );
    text.push_str(&prompt::instructions(previous_summary.is_some()));
    vec![Message::user(text)]
}

/// Run the pipeline. See the module docs for the stage order.
pub async fn run(
    input: PipelineInput,
    cfg: &PipelineConfig,
    summarizer: &dyn Summarizer,
) -> PipelineOutput {
    let threshold = cfg.threshold_tokens();
    let target = cfg.target_tokens();
    let base = cfg.overhead_tokens;
    let prev_body: Option<String> = input
        .previous_summary
        .as_deref()
        .map(|s| split_pinned(s).0.to_string());

    let mut summary = input.previous_summary.clone();
    let mut messages = input.messages.clone();
    let tokens_before = base
        .saturating_add(summary_tokens(&summary))
        .saturating_add(messages_tokens(&messages));
    let mut out = PipelineOutput {
        messages: input.messages.clone(),
        summary: input.previous_summary.clone(),
        folded: 0,
        method: None,
        tokens_before,
        tokens_after: tokens_before,
        rounds: 0,
        stubbed: 0,
        summary_failed: false,
    };
    if !cfg.force && tokens_before < threshold {
        return out;
    }

    // Round 1, stage 1: mechanical shake.
    let mut k = cfg.keep_recent_turns.max(1);
    let mut rounds = 1;
    let sh = shake(
        &messages,
        &ShakeParams {
            keep_recent_turns: k,
            min_chars: SHAKE_MIN_CHARS,
            target_tokens: target,
            overhead_tokens: base.saturating_add(summary_tokens(&summary)),
        },
    );
    let mut stubbed = sh.stubbed;
    messages = sh.messages;
    let mut tokens = sh.tokens;
    let mut method = (stubbed > 0).then_some(CompactionMethod::Shake);

    // Stage 2 + 3: summary of the older part with pinned blocks, only if still needed.
    let mut folded = 0;
    let mut summary_failed = false;
    if cfg.force || tokens >= threshold {
        let (older, keep) = split_for_compaction(&messages, cfg.keep_recent_messages);
        if !older.is_empty() {
            folded = older.len();
            // Summarise the originals (same indices; shake never changes the length).
            let span = &input.messages[..folded];
            let request = build_summary_request(prev_body.as_deref(), span);
            let body = match summarizer.summarize(request).await {
                Ok(text) if !text.trim().is_empty() => {
                    method = Some(CompactionMethod::Summary);
                    text.trim().to_string()
                }
                Ok(_) | Err(_) => {
                    summary_failed = true;
                    method = Some(CompactionMethod::Shake);
                    let mut fallback = prev_body.clone().unwrap_or_default();
                    if !fallback.is_empty() {
                        fallback.push_str("\n\n");
                    }
                    fallback.push_str(&digest(span));
                    truncate_head_tail(&fallback, FALLBACK_BODY_CAP)
                }
            };
            summary = Some(attach_pinned(&body, &input.pinned));
            messages = keep;
            tokens = base
                .saturating_add(summary_tokens(&summary))
                .saturating_add(messages_tokens(&messages));
        }
    }

    // Stage 4: validation. Still over target: shrink K and shake once more.
    if tokens > target && rounds < MAX_ROUNDS && k > 1 {
        rounds += 1;
        k = (k / 2).max(1);
        let sh = shake(
            &messages,
            &ShakeParams {
                keep_recent_turns: k,
                min_chars: SHAKE_MIN_CHARS,
                target_tokens: target,
                overhead_tokens: base.saturating_add(summary_tokens(&summary)),
            },
        );
        if sh.stubbed > 0 {
            stubbed += sh.stubbed;
            messages = sh.messages;
            tokens = sh.tokens;
            method = method.or(Some(CompactionMethod::Shake));
        }
    }

    out.messages = messages;
    out.summary = summary;
    out.folded = folded;
    out.method = method;
    out.tokens_after = tokens;
    out.rounds = rounds;
    out.stubbed = stubbed;
    out.summary_failed = summary_failed;
    out
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod pipeline_tests;
