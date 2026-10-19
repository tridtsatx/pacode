//! Splitting one provider request into categories.
//!
//! The provider reports a single `input_tokens` number for the whole prompt and
//! nothing finer. The split below is therefore built from the same pieces the
//! request was assembled from, each run through pacode's chars/4 estimator —
//! the identical estimator the context window and `/compact` budget already
//! use, so the page cannot disagree with the thing it is measuring. Only
//! `reply` is a real count: it is the provider's own `output_tokens`.

use std::sync::Arc;

use pacode_types::{
    CategoryTotals, ContentBlock, Message, ToolDefinition, Usage, estimate_tokens_for_chars,
};

/// What one request cost, split by category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestAttribution {
    pub categories: CategoryTotals,
    /// Provider-reported input for the request; 0 when it reported none.
    pub reported_input: u64,
    /// Provider-reported output, excluding reasoning. `categories.reply` adds
    /// the reasoning tokens on top, matching how `ModelInfo::cost` bills.
    pub reported_output: u64,
    pub cache_read: u64,
}

impl RequestAttribution {
    /// Attribute one assembled `CompletionRequest`.
    ///
    /// `history` must be the same slice the request carried — `Agent::history`
    /// is *not* it, since `for_model()` is what filters it. Passing a superset
    /// would bill the model for messages it was never sent.
    ///
    /// `reply` folds reasoning into the reply, matching how `ModelInfo::cost`
    /// bills a request.
    ///
    /// The split itself needs nothing but the request, so it can be taken
    /// *before* the call — which is the only place the caller's copy of the
    /// history still exists. The reported counts are then attached by
    /// [`Self::with_usage`], because they are only known once the model has
    /// answered and the request has been consumed.
    pub fn from_request(
        system_static: &str,
        system_dynamic: &str,
        tools: &[ToolDefinition],
        history: &[Arc<Message>],
    ) -> Self {
        let (result_chars, conversation_chars) = split_history(history);
        let categories = CategoryTotals {
            instructions: chars(system_static) + chars(system_dynamic),
            tool_defs: tools
                .iter()
                .map(|tool| u64::from(tool.estimate_tokens()))
                .fold(0, u64::saturating_add),
            history: estimate(conversation_chars),
            tool_results: estimate(result_chars),
            reply: 0,
        };
        Self {
            categories,
            reported_input: 0,
            reported_output: 0,
            cache_read: 0,
        }
    }

    /// Attach what the provider reported, and fold reasoning into the reply.
    pub fn with_usage(mut self, usage: &Usage) -> Self {
        self.categories.reply = usage.output_tokens.saturating_add(usage.reasoning_tokens);
        self.reported_input = usage.input_tokens;
        self.reported_output = usage.output_tokens;
        self.cache_read = usage.cache_read_tokens;
        self
    }
}

/// Kept for a caller that has both halves at once — a test, or a provider that
/// reports usage before the request is dropped.
impl RequestAttribution {
    #[allow(dead_code)]
    pub fn from_request_with_usage(
        system_static: &str,
        system_dynamic: &str,
        tools: &[ToolDefinition],
        history: &[Arc<Message>],
        usage: &Usage,
    ) -> Self {
        Self::from_request(system_static, system_dynamic, tools, history).with_usage(usage)
    }
}

fn chars(text: &str) -> u64 {
    u64::from(estimate_tokens_for_chars(text.chars().count()))
}

fn estimate(char_count: usize) -> u64 {
    u64::from(estimate_tokens_for_chars(char_count))
}

/// `(tool result chars, everything-else chars)` for the history the request
/// carried.
///
/// Each side is estimated on its own, so the two can exceed
/// `Message::estimate_tokens` for the same messages by up to four tokens of
/// rounding. The char count itself comes from [`Message::char_len`] rather than
/// a second copy of its block table, so the page cannot drift from the number
/// the compaction budget uses.
fn split_history(history: &[Arc<Message>]) -> (usize, usize) {
    let mut result_chars = 0usize;
    let mut conversation_chars = 0usize;
    for message in history {
        let message_result_chars: usize = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content.chars().count()),
                ContentBlock::Text { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolUse { .. }
                | ContentBlock::Image { .. } => None,
            })
            .sum();
        result_chars = result_chars.saturating_add(message_result_chars);
        conversation_chars = conversation_chars
            .saturating_add(message.char_len())
            .saturating_sub(message_result_chars);
    }
    (result_chars, conversation_chars)
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod attribution_tests;
