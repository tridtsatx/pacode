//! Compaction (spec §6.4). Port the approach of jcode `jcode-compaction-core`.

use std::sync::Arc;

use codeapp_types::Message;

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

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
    let _ = (messages, keep_recent);
    todo!("compaction::split_for_compaction")
}

/// The summarisation prompt: previous summary (if any) + the messages to fold in.
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
) -> Vec<Message> {
    let _ = (previous_summary, messages);
    todo!("compaction::build_summary_request")
}

/// Run the summary turn with `context.compaction_model` (or the agent's model), replace
/// the history, persist, emit a `Notice` item. Returns false when nothing was done.
pub async fn compact(session: &Arc<Session>, agent: &Arc<Agent>) -> Result<bool, CoreError> {
    let _ = (session, agent);
    todo!("compaction::compact")
}
