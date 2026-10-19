//! Server-Sent Events (SSE) parser and chunk-to-events translation for OpenAI-compatible streams.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{CallId, Effort, StopReason, StreamEvent};
use serde_json::Value;

use crate::retry::RetryAfterSignal;
use crate::{EventStream, ProviderError};

#[cfg(test)]
#[path = "sse_tests.rs"]
mod sse_tests;

pub(crate) const MAX_SSE_BUFFER_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_SSE_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_PENDING_TOOL_CALLS: usize = 256;
pub(crate) const MAX_BUFFERED_ARGS_BYTES: usize = 1024 * 1024;

/// Incremental SSE parser: feed raw bytes, get complete `data:` payloads.
pub struct SseParser {
    buffer: Vec<u8>,
    /// Bytes of `buffer` already searched without finding a delimiter, so a large event
    /// arriving in many small chunks is scanned once, not once per chunk.
    scan_from: usize,
    max_buffer_bytes: usize,
}

impl SseParser {
    pub fn new() -> Self {
        Self::with_max_buffer_bytes(MAX_SSE_BUFFER_BYTES)
    }

    pub(crate) fn with_max_buffer_bytes(max_buffer_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            scan_from: 0,
            max_buffer_bytes,
        }
    }

    /// Append bytes; returns the `data:` payloads of every complete event (an
    /// over-budget event is dropped, see [`Self::feed_checked`]).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.feed_checked(bytes).unwrap_or_default()
    }

    /// Append bytes; returns the `data:` payloads of every complete event, or an error
    /// once a single event outgrows the buffer budget.
    pub(crate) fn feed_checked(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        if self.buffer.len().saturating_add(bytes.len()) > self.max_buffer_bytes {
            self.buffer.clear();
            self.scan_from = 0;
            return Err(ProviderError::Malformed(format!(
                "SSE event exceeds {}-byte buffer budget",
                self.max_buffer_bytes
            )));
        }
        self.buffer.extend_from_slice(bytes);

        let mut events = Vec::new();
        let mut cursor = 0;
        // A delimiter is at most 4 bytes, so resume 3 bytes before the previous end.
        let mut resume = self.scan_from.saturating_sub(3);

        while cursor < self.buffer.len() {
            let remaining = &self.buffer[cursor..];
            match find_event_delimiter(remaining, resume) {
                Some((offset, delim_len)) => {
                    let event_bytes = &remaining[..offset];
                    let event_str = String::from_utf8_lossy(event_bytes);
                    if let Some(payload) = parse_sse_event_block(&event_str) {
                        events.push(payload);
                    }
                    cursor += offset + delim_len;
                    resume = 0;
                }
                None => break,
            }
        }

        self.buffer.drain(..cursor);
        self.scan_from = self.buffer.len();
        Ok(events)
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// First event delimiter in `slice` at or after `from`.
fn find_event_delimiter(slice: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
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

/// Cap for the text extracted from an in-stream `error` object (spec §6.4: everything
/// that can reach the model context or the UI is bounded).
pub(crate) const MAX_STREAM_ERROR_TEXT_BYTES: usize = 2000;

/// Classify an `error` object carried *inside* an SSE chunk.
///
/// Gateways (OpenRouter-style pools, opencode zen, ...) answer with HTTP 200 and then
/// push the upstream failure as `{"error":{"message":..,"code":429,"metadata":{"raw":..}}}`.
/// Mapping the numeric `code` back onto [`ProviderError`] is what makes a mid-stream 429
/// retryable again, and `metadata.raw` is the only place the useful upstream text lives.
pub fn stream_error_to_provider_error(err_val: &Value) -> ProviderError {
    let message = match err_val.get("message").and_then(|v| v.as_str()) {
        Some(msg) => msg.to_string(),
        None => match err_val.as_str() {
            Some(s) => s.to_string(),
            None => err_val.to_string(),
        },
    };

    let detail = match err_val.get("metadata").and_then(|m| m.get("raw")) {
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed == message {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other @ (Value::Object(_) | Value::Array(_))) => Some(other.to_string()),
        Some(Value::Null | Value::Bool(_) | Value::Number(_)) | None => None,
    };

    let text = match detail {
        Some(raw) => {
            truncate_on_char_boundary(&format!("{message}: {raw}"), MAX_STREAM_ERROR_TEXT_BYTES)
        }
        None => truncate_on_char_boundary(&message, MAX_STREAM_ERROR_TEXT_BYTES),
    };

    let code = err_val.get("code").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
    });

    match code {
        Some(status @ (401 | 403)) => ProviderError::Auth(format!("{status}: {text}")),
        Some(429) => ProviderError::RateLimited(text),
        Some(status) if (400..=599).contains(&status) => ProviderError::Http {
            status: status as u16,
            message: text,
        },
        Some(_) | None => ProviderError::Malformed(text),
    }
}

fn truncate_on_char_boundary(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &input[..end])
}

/// Translate one chat-completions chunk (already parsed JSON) into stream events.
/// `state` carries tool-call bookkeeping across chunks.
pub fn chunk_to_events(
    chunk: &Value,
    state: &mut ChunkState,
) -> Result<Vec<StreamEvent>, ProviderError> {
    if let Some(err_val) = chunk.get("error") {
        return Err(stream_error_to_provider_error(err_val));
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
                    let func = tc.get("function");
                    crate::sse_typed::push_tool_call(
                        state,
                        index,
                        tc.get("id").and_then(|v| v.as_str()),
                        func.and_then(|f| f.get("name")).and_then(|v| v.as_str()),
                        func.and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str()),
                        &mut events,
                    );
                }
            }
        }

        if let Some(finish_reason) = first_choice.get("finish_reason").and_then(|v| v.as_str()) {
            let stop = crate::sse_typed::stop_reason(finish_reason);
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

        events.push(crate::sse_typed::usage_event(
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
        ));
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

pub(crate) struct ResponseReadBudget {
    limit: usize,
    used: usize,
}

impl ResponseReadBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }

    pub(crate) fn consume(&mut self, bytes: usize) -> Result<(), ProviderError> {
        let next = self.used.checked_add(bytes).ok_or_else(|| {
            ProviderError::Malformed("SSE response byte count overflowed".to_string())
        })?;
        if next > self.limit {
            return Err(ProviderError::Malformed(format!(
                "SSE response exceeds {}-byte read budget",
                self.limit
            )));
        }
        self.used = next;
        Ok(())
    }
}

pub(crate) fn retry_after_from_error(error: &Value) -> Option<Duration> {
    let metadata = error.get("metadata");
    let direct = error
        .get("retry_after")
        .or_else(|| error.get("retryAfter"))
        .or_else(|| metadata.and_then(|value| value.get("retry_after")))
        .or_else(|| metadata.and_then(|value| value.get("retryAfter")));
    if let Some(delay) = direct.and_then(crate::retry::retry_after_value) {
        return Some(delay);
    }

    let headers = error
        .get("response_headers")
        .or_else(|| error.get("responseHeaders"))
        .or_else(|| metadata.and_then(|value| value.get("response_headers")))
        .or_else(|| metadata.and_then(|value| value.get("responseHeaders")))?
        .as_object()?;
    headers
        .iter()
        .find(|(name, _)| name.as_str().eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| crate::retry::retry_after_value(value))
}

/// Finish an error produced by [`crate::sse_typed::payload_to_events`] for `payload`.
/// Error path only, so the extra `Value` parse never touches the hot path: a rate
/// limit reported inside the stream publishes its `retry_after` hint, and invalid JSON
/// names what the gateway actually sent.
fn stream_error(
    err: ProviderError,
    payload: &str,
    retry_after: &RetryAfterSignal,
) -> ProviderError {
    match err {
        ProviderError::RateLimited(_) => {
            if let Some(delay) = serde_json::from_str::<Value>(payload)
                .ok()
                .as_ref()
                .and_then(|json| json.get("error"))
                .and_then(retry_after_from_error)
            {
                retry_after.set(delay);
            }
            err
        }
        ProviderError::Malformed(msg) if msg.starts_with("invalid SSE JSON") => {
            ProviderError::Malformed(format!("{msg}: {}", crate::error_body_message(payload)))
        }
        other => other,
    }
}

pub(crate) fn create_event_stream_with_retry_after(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
    retry_after: RetryAfterSignal,
) -> EventStream {
    create_event_stream_with_limits(
        response,
        req_model,
        stream_idle_secs,
        effort,
        retry_after,
        MAX_SSE_RESPONSE_BYTES,
    )
}

fn create_event_stream_with_limits(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
    retry_after: RetryAfterSignal,
    max_response_bytes: usize,
) -> EventStream {
    let timeout_secs = stream_idle_secs.saturating_mul(effort.idle_timeout_factor());
    let idle_duration = (stream_idle_secs > 0).then(|| Duration::from_secs(timeout_secs.max(1)));

    struct StreamState<S> {
        byte_stream: S,
        parser: SseParser,
        chunk_state: ChunkState,
        pending_events: VecDeque<StreamEvent>,
        /// Error to surface once `pending_events` is drained: events parsed from earlier
        /// payloads of the same byte chunk must not be lost because a later payload failed.
        pending_error: Option<ProviderError>,
        model: Option<String>,
        message_start_emitted: bool,
        stream_ended: bool,
        idle_duration: Option<Duration>,
        timeout_secs: u64,
        read_budget: ResponseReadBudget,
        retry_after: RetryAfterSignal,
    }

    let initial_state = StreamState {
        byte_stream: Box::pin(response.bytes_stream()),
        parser: SseParser::new(),
        chunk_state: ChunkState::default(),
        pending_events: VecDeque::new(),
        pending_error: None,
        model: Some(req_model),
        message_start_emitted: false,
        stream_ended: false,
        idle_duration,
        timeout_secs,
        read_budget: ResponseReadBudget::new(max_response_bytes),
        retry_after,
    };

    let stream = futures::stream::unfold(initial_state, |mut state| async move {
        loop {
            if let Some(event) = state.pending_events.pop_front() {
                return Some((Ok(event), state));
            }

            if let Some(err) = state.pending_error.take() {
                state.stream_ended = true;
                return Some((Err(err), state));
            }

            if state.stream_ended {
                return None;
            }

            let next_chunk = match state.idle_duration {
                Some(idle_duration) => {
                    tokio::time::timeout(idle_duration, state.byte_stream.next()).await
                }
                None => Ok(state.byte_stream.next().await),
            };

            match next_chunk {
                Err(_) => {
                    state.stream_ended = true;
                    return Some((Err(ProviderError::IdleTimeout(state.timeout_secs)), state));
                }
                Ok(Some(Ok(bytes))) => {
                    if let Err(error) = state.read_budget.consume(bytes.len()) {
                        state.stream_ended = true;
                        state.pending_error = Some(error);
                        continue;
                    }
                    // The lossy copy + redact scan run per network chunk — only
                    // when `trace` is actually on (off in release builds).
                    if log::log_enabled!(log::Level::Trace) {
                        let chunk_str = String::from_utf8_lossy(&bytes);
                        let chunk_redacted = crate::redact(&chunk_str);
                        log::trace!("response body chunk: {chunk_redacted}");
                    }
                    let payloads = match state.parser.feed_checked(bytes.as_ref()) {
                        Ok(payloads) => payloads,
                        Err(error) => {
                            state.stream_ended = true;
                            state.pending_error = Some(error);
                            continue;
                        }
                    };
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

                        match crate::sse_typed::payload_to_events(
                            &payload,
                            &mut state.chunk_state,
                            &mut state.model,
                        ) {
                            Err(err) => {
                                state.stream_ended = true;
                                state.pending_error =
                                    Some(stream_error(err, &payload, &state.retry_after));
                                break;
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
                                        state.pending_events.push_back(StreamEvent::MessageStart {
                                            model: state.model.clone(),
                                        });
                                        state.message_start_emitted = true;
                                    }
                                    state.pending_events.push_back(event);
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
