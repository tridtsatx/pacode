//! Server-Sent Events (SSE) parser and chunk-to-events translation for OpenAI-compatible streams.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use codeapp_types::{CallId, Effort, StopReason, StreamEvent, Usage};
use futures::StreamExt;
use serde_json::Value;

use crate::{EventStream, ProviderError};

#[cfg(test)]
#[path = "sse_tests.rs"]
mod sse_tests;

pub(crate) const MAX_SSE_BUFFER_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_PENDING_TOOL_CALLS: usize = 256;
pub(crate) const MAX_BUFFERED_ARGS_BYTES: usize = 1024 * 1024;

/// Incremental SSE parser: feed raw bytes, get complete `data:` payloads.
pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Append bytes; returns the `data:` payloads of every complete event.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_SSE_BUFFER_BYTES {
            self.buffer.clear();
            return Vec::new();
        }

        let mut events = Vec::new();
        let mut cursor = 0;

        while cursor < self.buffer.len() {
            let remaining = &self.buffer[cursor..];
            match find_event_delimiter(remaining) {
                Some((offset, delim_len)) => {
                    let event_bytes = &remaining[..offset];
                    let event_str = String::from_utf8_lossy(event_bytes);
                    if let Some(payload) = parse_sse_event_block(&event_str) {
                        events.push(payload);
                    }
                    cursor += offset + delim_len;
                }
                None => break,
            }
        }

        self.buffer.drain(..cursor);
        events
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

fn find_event_delimiter(slice: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < slice.len() {
        if slice[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        } else if slice[i..].starts_with(b"\r\n\n") || slice[i..].starts_with(b"\n\r\n") {
            return Some((i, 3));
        } else if slice[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
        i += 1;
    }
    None
}

fn parse_sse_event_block(event_str: &str) -> Option<String> {
    let mut data_lines = Vec::new();
    for line in event_str.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(value);
        } else if line == "data" {
            data_lines.push("");
        } else if line.starts_with("event:")
            || line.starts_with("id:")
            || line.starts_with("retry:")
        {
            continue;
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

/// Translate one chat-completions chunk (already parsed JSON) into stream events.
/// `state` carries tool-call bookkeeping across chunks.
pub fn chunk_to_events(
    chunk: &Value,
    state: &mut ChunkState,
) -> Result<Vec<StreamEvent>, ProviderError> {
    if let Some(err_val) = chunk.get("error") {
        let message = if let Some(msg) = err_val.get("message").and_then(|v| v.as_str()) {
            msg.to_string()
        } else if let Some(s) = err_val.as_str() {
            s.to_string()
        } else {
            err_val.to_string()
        };
        return Err(ProviderError::Malformed(message));
    }

    let mut events = Vec::new();

    if let Some(choices) = chunk.get("choices").and_then(|v| v.as_array())
        && let Some(first_choice) = choices.first()
    {
        if let Some(delta) = first_choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                events.push(StreamEvent::TextDelta {
                    text: text.to_string(),
                });
            }

            let reasoning = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .or_else(|| delta.get("thinking"))
                .and_then(|v| v.as_str());
            if let Some(reasoning_text) = reasoning
                && !reasoning_text.is_empty()
            {
                events.push(StreamEvent::ReasoningDelta {
                    text: reasoning_text.to_string(),
                });
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for (pos, tc) in tool_calls.iter().enumerate() {
                    let index = tc
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .map(|i| i as u32)
                        .unwrap_or(pos as u32);

                    let id_opt = tc.get("id").and_then(|v| v.as_str()).map(CallId::new);
                    let func = tc.get("function");
                    let name_opt = func
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    let args_opt = func
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());

                    if state.started_indices.contains(&index) {
                        if let Some(args) = args_opt {
                            events.push(StreamEvent::ToolCallArgsDelta {
                                index,
                                delta: args.to_string(),
                            });
                        }
                    } else if state.pending_calls.len() < MAX_PENDING_TOOL_CALLS {
                        let pending = state.pending_calls.entry(index).or_default();
                        if let Some(id) = id_opt {
                            pending.id = Some(id);
                        }
                        if let Some(name) = name_opt {
                            pending.name = Some(name);
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
                            if let Some(args) = args_opt {
                                events.push(StreamEvent::ToolCallArgsDelta {
                                    index,
                                    delta: args.to_string(),
                                });
                            }
                        } else if let Some(args) = args_opt
                            && pending.buffered_args.len() + args.len() <= MAX_BUFFERED_ARGS_BYTES
                        {
                            pending.buffered_args.push_str(args);
                        }
                    }
                }
            }
        }

        if let Some(finish_reason) = first_choice.get("finish_reason").and_then(|v| v.as_str()) {
            let stop = match finish_reason {
                "stop" => StopReason::EndTurn,
                "tool_calls" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::ContentFilter,
                other => StopReason::Other(other.to_string()),
            };
            events.push(StreamEvent::MessageEnd { stop });
            state.message_end_emitted = true;
        }
    }

    if let Some(usage_val) = chunk.get("usage").filter(|v| v.is_object()) {
        let input_tokens = usage_val
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage_val
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let reasoning_tokens = usage_val
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read_tokens = usage_val
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        events.push(StreamEvent::Usage(Usage {
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_write_tokens: 0,
        }));
    }

    Ok(events)
}

#[derive(Default)]
pub struct ChunkState {
    pub(crate) started_indices: BTreeSet<u32>,
    pub(crate) pending_calls: BTreeMap<u32, PendingToolCall>,
    pub(crate) any_tool_call: bool,
    pub(crate) message_end_emitted: bool,
}

impl ChunkState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn any_tool_call(&self) -> bool {
        self.any_tool_call
    }

    pub fn message_end_emitted(&self) -> bool {
        self.message_end_emitted
    }

    pub fn is_started(&self, index: u32) -> bool {
        self.started_indices.contains(&index)
    }
}

#[derive(Default)]
pub(crate) struct PendingToolCall {
    pub(crate) id: Option<CallId>,
    pub(crate) name: Option<String>,
    pub(crate) buffered_args: String,
}

pub(crate) fn create_event_stream(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
) -> EventStream {
    let timeout_secs = stream_idle_secs * effort.idle_timeout_factor();
    let idle_duration = Duration::from_secs(timeout_secs.max(1));

    struct StreamState<S> {
        byte_stream: S,
        parser: SseParser,
        chunk_state: ChunkState,
        pending_events: VecDeque<StreamEvent>,
        model: Option<String>,
        message_start_emitted: bool,
        stream_ended: bool,
        idle_duration: Duration,
        timeout_secs: u64,
    }

    let initial_state = StreamState {
        byte_stream: Box::pin(response.bytes_stream()),
        parser: SseParser::new(),
        chunk_state: ChunkState::default(),
        pending_events: VecDeque::new(),
        model: Some(req_model),
        message_start_emitted: false,
        stream_ended: false,
        idle_duration,
        timeout_secs,
    };

    let stream = futures::stream::unfold(initial_state, |mut state| async move {
        loop {
            if let Some(event) = state.pending_events.pop_front() {
                return Some((Ok(event), state));
            }

            if state.stream_ended {
                return None;
            }

            let next_chunk =
                tokio::time::timeout(state.idle_duration, state.byte_stream.next()).await;

            match next_chunk {
                Err(_) => {
                    state.stream_ended = true;
                    return Some((Err(ProviderError::IdleTimeout(state.timeout_secs)), state));
                }
                Ok(Some(Ok(bytes))) => {
                    let payloads = state.parser.feed(bytes.as_ref());
                    for payload in payloads {
                        if payload.trim() == "[DONE]" {
                            state.stream_ended = true;
                            if !state.chunk_state.message_end_emitted {
                                let stop = if state.chunk_state.any_tool_call {
                                    StopReason::ToolUse
                                } else {
                                    StopReason::EndTurn
                                };
                                state
                                    .pending_events
                                    .push_back(StreamEvent::MessageEnd { stop });
                                state.chunk_state.message_end_emitted = true;
                            }
                            break;
                        }

                        match serde_json::from_str::<Value>(&payload) {
                            Err(e) => {
                                state.stream_ended = true;
                                return Some((
                                    Err(ProviderError::Malformed(format!("invalid SSE JSON: {e}"))),
                                    state,
                                ));
                            }
                            Ok(json) => {
                                if let Some(m) = json.get("model").and_then(|v| v.as_str()) {
                                    state.model = Some(m.to_string());
                                }

                                match chunk_to_events(&json, &mut state.chunk_state) {
                                    Err(err) => {
                                        state.stream_ended = true;
                                        return Some((Err(err), state));
                                    }
                                    Ok(events) => {
                                        for event in events {
                                            if !state.message_start_emitted
                                                && matches!(
                                                    event,
                                                    StreamEvent::TextDelta { .. }
                                                        | StreamEvent::ReasoningDelta { .. }
                                                        | StreamEvent::ToolCallStart { .. }
                                                        | StreamEvent::ToolCallArgsDelta { .. }
                                                )
                                            {
                                                state.pending_events.push_back(
                                                    StreamEvent::MessageStart {
                                                        model: state.model.clone(),
                                                    },
                                                );
                                                state.message_start_emitted = true;
                                            }
                                            state.pending_events.push_back(event);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(err))) => {
                    state.stream_ended = true;
                    return Some((Err(ProviderError::Transport(err.to_string())), state));
                }
                Ok(None) => {
                    state.stream_ended = true;
                    if !state.chunk_state.message_end_emitted {
                        let stop = if state.chunk_state.any_tool_call {
                            StopReason::ToolUse
                        } else {
                            StopReason::EndTurn
                        };
                        state
                            .pending_events
                            .push_back(StreamEvent::MessageEnd { stop });
                        state.chunk_state.message_end_emitted = true;
                    }
                }
            }
        }
    });

    Box::pin(stream)
}
