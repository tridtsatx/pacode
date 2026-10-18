//! Stage 1 of compaction: mechanical shake. Old, large tool results are replaced by a
//! short stub (tool name, key argument, size, head/tail excerpt). No model call.
//!
//! Safety rules: results of the last K user turns are never touched; the latest result
//! per distinct file path is never touched; pinned messages (non-`Normal` kind) and
//! results of pinned tools are never touched; the message list keeps its length and
//! every `ToolResult` keeps its `call_id`, so tool_use/tool_result pairs cannot split.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pacode_types::{CallId, ContentBlock, Message, MessageKind, Role};
use serde_json::Value;

/// Tools whose results carry state the agent must keep verbatim.
pub const PINNED_TOOLS: &[&str] = &["plan", "ask_question", "skill", "report_status"];

/// Every stub starts with this text; used to keep shaking idempotent.
pub const STUB_PREFIX: &str = "[shaken tool result:";

/// Characters kept from the head and from the tail of a shaken result.
const EXCERPT_CHARS: usize = 240;

/// Input keys that identify what a tool call acted on, in priority order.
const KEY_ARGS: &[&str] = &["path", "file_path", "command", "pattern", "url", "query"];

#[derive(Clone, Debug)]
pub struct ShakeParams {
    /// Results within the last N user turns are never shaken.
    pub keep_recent_turns: usize,
    /// Only results with at least this many characters are shaken.
    pub min_chars: usize,
    /// Stop shaking once the estimate (incl. `overhead_tokens`) is at or below this.
    pub target_tokens: u32,
    /// Tokens outside the message list (system prompt, tools, summary) in the estimate.
    pub overhead_tokens: u32,
}

#[derive(Clone, Debug)]
pub struct ShakeOutcome {
    pub messages: Vec<Arc<Message>>,
    /// Number of tool results replaced by stubs.
    pub stubbed: usize,
    /// Estimated tokens after shaking (`overhead_tokens` included).
    pub tokens: u32,
}

/// A real user turn starts at a normal user message (not an injection or reminder).
fn is_turn_start(msg: &Message) -> bool {
    msg.role == Role::User && msg.meta.kind == MessageKind::Normal
}

/// Index of the first message of the K-th most recent turn; everything from here on is
/// protected. Fewer than K turns means everything is protected (returns 0).
pub fn protected_from(messages: &[Arc<Message>], keep_recent_turns: usize) -> usize {
    let k = keep_recent_turns.max(1);
    let mut seen = 0;
    for (i, msg) in messages.iter().enumerate().rev() {
        if is_turn_start(msg) {
            seen += 1;
            if seen == k {
                return i;
            }
        }
    }
    0
}

pub(super) type CallMap<'a> = HashMap<&'a CallId, (&'a str, &'a Value)>;

/// call id -> (tool name, input) for every tool call in the history.
pub(super) fn collect_calls(messages: &[Arc<Message>]) -> CallMap<'_> {
    let mut calls = HashMap::new();
    for msg in messages {
        for (id, name, input) in msg.tool_uses() {
            calls.insert(id, (name, input));
        }
    }
    calls
}

/// The file path a tool call acted on, if any.
fn path_of(input: &Value) -> Option<&str> {
    ["path", "file_path"]
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
}

/// Short description of the call's key argument (path, command, ...).
pub(super) fn key_arg(input: &Value) -> Option<String> {
    KEY_ARGS
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .map(|s| pacode_types::truncate_head_tail(&s.replace('\n', " "), 160))
}

fn make_stub(name: &str, key: Option<&str>, content: &str, is_error: bool) -> String {
    let chars = content.chars().count();
    let lines = content.lines().count();
    let key = key.map(|k| format!(" `{k}`")).unwrap_or_default();
    let err = if is_error { ", error" } else { "" };
    let header = format!("{STUB_PREFIX} {name}{key} | {chars} chars, {lines} lines{err}]");
    if chars <= EXCERPT_CHARS * 2 {
        return format!("{header}\n{content}");
    }
    let head: String = content.chars().take(EXCERPT_CHARS).collect();
    let tail: String = content.chars().skip(chars - EXCERPT_CHARS).collect();
    let omitted = chars - EXCERPT_CHARS * 2;
    format!("{header}\n{head}\n[... {omitted} chars omitted ...]\n{tail}")
}

/// Shake old, large tool results oldest-first until the estimate reaches the target.
pub fn shake(messages: &[Arc<Message>], params: &ShakeParams) -> ShakeOutcome {
    let cutoff = protected_from(messages, params.keep_recent_turns);
    let calls = collect_calls(messages);

    // Latest result per distinct file path is exempt.
    let mut latest_for_path: HashMap<&str, usize> = HashMap::new();
    for (i, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            if let ContentBlock::ToolResult { call_id, .. } = block
                && let Some((_, input)) = calls.get(call_id)
                && let Some(path) = path_of(input)
            {
                latest_for_path.insert(path, i);
            }
        }
    }
    let exempt: HashSet<usize> = latest_for_path.values().copied().collect();

    let mut out: Vec<Arc<Message>> = messages.to_vec();
    let mut tokens = messages
        .iter()
        .map(|m| m.estimate_tokens())
        .fold(params.overhead_tokens, |a, t| a.saturating_add(t));
    let mut stubbed = 0;

    for (i, msg) in messages.iter().enumerate().take(cutoff) {
        if tokens <= params.target_tokens {
            break;
        }
        if msg.role != Role::Tool || msg.meta.kind != MessageKind::Normal || exempt.contains(&i) {
            continue;
        }
        let mut changed = false;
        let mut content = Vec::with_capacity(msg.content.len());
        for block in &msg.content {
            match block {
                ContentBlock::ToolResult {
                    call_id,
                    content: text,
                    is_error,
                } if text.chars().count() >= params.min_chars && !text.starts_with(STUB_PREFIX) => {
                    let (name, input) = calls
                        .get(call_id)
                        .map(|(n, v)| (*n, Some(*v)))
                        .unwrap_or(("unknown", None));
                    if PINNED_TOOLS.contains(&name) {
                        content.push(block.clone());
                        continue;
                    }
                    let key = input.and_then(key_arg);
                    content.push(ContentBlock::ToolResult {
                        call_id: call_id.clone(),
                        content: make_stub(name, key.as_deref(), text, *is_error),
                        is_error: *is_error,
                    });
                    changed = true;
                }
                // Old tool-result images are the first thing to go: they are big and
                // the text result (or its stub) carries the meaning.
                ContentBlock::Image { .. } => changed = true,
                ContentBlock::ToolResult { .. }
                | ContentBlock::Text { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolUse { .. } => content.push(block.clone()),
            }
        }
        if changed {
            let mut new_msg = (**msg).clone();
            new_msg.content = content;
            tokens = tokens
                .saturating_sub(msg.estimate_tokens())
                .saturating_add(new_msg.estimate_tokens());
            out[i] = Arc::new(new_msg);
            stubbed += 1;
        }
    }

    ShakeOutcome {
        messages: out,
        stubbed,
        tokens,
    }
}

#[cfg(test)]
#[path = "shake_tests.rs"]
mod shake_tests;
