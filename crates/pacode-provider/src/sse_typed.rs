//! Typed, borrowing fast path for chat-completions SSE chunks (finding P1-2).
//!
//! The streaming hot path parses every payload into [`Chunk`] (strings borrow from the
//! payload as `Cow`, escaped strings fall back to owned) instead of building a
//! `serde_json::Value` tree. Anything the typed shape does not accept, and every chunk
//! that carries an `error` key, is handed to the original `Value` translation
//! ([`crate::sse::chunk_to_events`]), which stays the oracle and the classifier of
//! in-stream errors. Both paths share the stateful helpers below, so tool-call
//! bookkeeping cannot diverge.
//!
//! Nothing here caches or holds state beyond the caller's [`ChunkState`].

use std::borrow::Cow;

use pacode_types::{CallId, StopReason, StreamEvent, Usage};
use serde::Deserialize;

use crate::ProviderError;
use crate::sse::{ChunkState, MAX_BUFFERED_ARGS_BYTES, MAX_PENDING_TOOL_CALLS};

#[cfg(test)]
#[path = "sse_typed_tests.rs"]
mod sse_typed_tests;

/// Accepts (and discards) any JSON value; its presence marks a key as set.
struct Present;

impl<'de> Deserialize<'de> for Present {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d).map(|_| Present)
    }
}

#[derive(Deserialize)]
struct Chunk<'a> {
    #[serde(borrow)]
    choices: Option<Vec<Choice<'a>>>,
    usage: Option<UsageWire>,
    /// Only presence matters: such chunks are re-parsed on the `Value` path.
    error: Option<Present>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Choice<'a> {
    #[serde(borrow)]
    delta: Option<Delta<'a>>,
    #[serde(borrow)]
    finish_reason: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Delta<'a> {
    #[serde(borrow)]
    content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    reasoning_content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    reasoning: Option<Cow<'a, str>>,
    #[serde(borrow)]
    thinking: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool_calls: Option<Vec<ToolCallWire<'a>>>,
}

#[derive(Deserialize)]
struct ToolCallWire<'a> {
    index: Option<u64>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    function: Option<FunctionWire<'a>>,
}

#[derive(Deserialize)]
struct FunctionWire<'a> {
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    arguments: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct UsageWire {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    completion_tokens_details: Option<CompletionDetails>,
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Deserialize)]
struct CompletionDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

/// Translate one SSE `data:` payload (not `[DONE]`) into stream events.
///
/// `model` is updated only when the chunk names a different model than the one held.
/// Invalid JSON yields `Malformed("invalid SSE JSON: ..")`, as on the `Value` path.
pub(crate) fn payload_to_events(
    payload: &str,
    state: &mut ChunkState,
    model: &mut Option<String>,
) -> Result<Vec<StreamEvent>, ProviderError> {
    if let Ok(chunk) = serde_json::from_str::<Chunk>(payload)
        && chunk.error.is_none()
    {
        if let Some(m) = chunk.model.as_deref()
            && model.as_deref() != Some(m)
        {
            *model = Some(m.to_string());
        }
        return Ok(chunk_events(&chunk, state));
    }
    value_path(payload, state, model)
}

/// The original untyped translation; also the test oracle.
pub(crate) fn value_path(
    payload: &str,
    state: &mut ChunkState,
    model: &mut Option<String>,
) -> Result<Vec<StreamEvent>, ProviderError> {
    let json = serde_json::from_str::<serde_json::Value>(payload)
        .map_err(|e| ProviderError::Malformed(format!("invalid SSE JSON: {e}")))?;
    if let Some(m) = json.get("model").and_then(|v| v.as_str())
        && model.as_deref() != Some(m)
    {
        *model = Some(m.to_string());
    }
    crate::sse::chunk_to_events(&json, state)
}

fn chunk_events(chunk: &Chunk<'_>, state: &mut ChunkState) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    if let Some(first) = chunk.choices.as_ref().and_then(|c| c.first()) {
        if let Some(delta) = &first.delta {
            if let Some(text) = delta.content.as_deref().filter(|t| !t.is_empty()) {
                events.push(StreamEvent::TextDelta {
                    text: text.to_string(),
                });
            }
            let reasoning = delta
                .reasoning_content
                .as_deref()
                .or(delta.reasoning.as_deref())
                .or(delta.thinking.as_deref());
            if let Some(text) = reasoning.filter(|t| !t.is_empty()) {
                events.push(StreamEvent::ReasoningDelta {
                    text: text.to_string(),
                });
            }
            for (pos, tc) in delta.tool_calls.iter().flatten().enumerate() {
                let index = tc.index.map(|i| i as u32).unwrap_or(pos as u32);
                let func = tc.function.as_ref();
                push_tool_call(
                    state,
                    index,
                    tc.id.as_deref(),
                    func.and_then(|f| f.name.as_deref()),
                    func.and_then(|f| f.arguments.as_deref()),
                    &mut events,
                );
            }
        }
        if let Some(reason) = first.finish_reason.as_deref() {
            events.push(StreamEvent::MessageEnd {
                stop: stop_reason(reason),
            });
            state.message_end_emitted = true;
        }
    }

    if let Some(u) = &chunk.usage {
        events.push(usage_event(
            u.prompt_tokens.unwrap_or(0),
            u.completion_tokens.unwrap_or(0),
            u.completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)
                .unwrap_or(0),
            u.prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0),
        ));
    }
    events
}

/// `finish_reason` wire string to [`StopReason`].
pub(crate) fn stop_reason(finish_reason: &str) -> StopReason {
    match finish_reason {
        "stop" => StopReason::EndTurn,
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Other(other.to_string()),
    }
}

pub(crate) fn usage_event(input: u64, output: u64, reasoning: u64, cache_read: u64) -> StreamEvent {
    StreamEvent::Usage(Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
    })
}

/// One `delta.tool_calls[]` entry: starts the call once its name is known (buffering
/// early argument fragments, bounded by [`MAX_BUFFERED_ARGS_BYTES`] and
/// [`MAX_PENDING_TOOL_CALLS`]) and streams argument deltas afterwards.
pub(crate) fn push_tool_call(
    state: &mut ChunkState,
    index: u32,
    id: Option<&str>,
    name: Option<&str>,
    args: Option<&str>,
    events: &mut Vec<StreamEvent>,
) {
    let name = name.filter(|s| !s.is_empty());
    let args = args.filter(|s| !s.is_empty());

    if state.started_indices.contains(&index) {
        if let Some(args) = args {
            events.push(StreamEvent::ToolCallArgsDelta {
                index,
                delta: args.to_string(),
            });
        }
        return;
    }
    if state.pending_calls.len() >= MAX_PENDING_TOOL_CALLS {
        return;
    }

    let pending = state.pending_calls.entry(index).or_default();
    if let Some(id) = id {
        pending.id = Some(CallId::new(id));
    }
    if let Some(name) = name {
        pending.name = Some(name.to_string());
    }
    if let Some(name) = &pending.name {
        let call_id = pending.id.clone().unwrap_or_else(CallId::generate);
        let tool_name = name.clone();
        let buffered = std::mem::take(&mut pending.buffered_args);
        state.started_indices.insert(index);
        state.any_tool_call = true;

        events.push(StreamEvent::ToolCallStart {
            index,
            id: call_id,
            name: tool_name,
        });
        if !buffered.is_empty() {
            events.push(StreamEvent::ToolCallArgsDelta {
                index,
                delta: buffered,
            });
        }
        if let Some(args) = args {
            events.push(StreamEvent::ToolCallArgsDelta {
                index,
                delta: args.to_string(),
            });
        }
    } else if let Some(args) = args
        && pending.buffered_args.len() + args.len() <= MAX_BUFFERED_ARGS_BYTES
    {
        pending.buffered_args.push_str(args);
    }
}
