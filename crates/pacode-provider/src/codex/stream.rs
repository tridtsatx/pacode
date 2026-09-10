//! Event stream construction and retry policy for OpenAI Responses API.

use std::collections::VecDeque;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{Effort, StopReason, StreamEvent};
use serde_json::Value;

use super::events::{ResponsesChunkState, responses_chunk_to_events};
use crate::sse::SseParser;
use crate::{EventStream, ProviderError};

/// Create an [`EventStream`] decoding Responses API SSE chunks into pacode [`StreamEvent`]s.
pub fn create_responses_event_stream(
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
        chunk_state: ResponsesChunkState,
        pending_events: VecDeque<StreamEvent>,
        pending_error: Option<ProviderError>,
        model: Option<String>,
        message_start_emitted: bool,
        stream_ended: bool,
        idle_duration: Duration,
        timeout_secs: u64,
    }

    let initial_state = StreamState {
        byte_stream: Box::pin(response.bytes_stream()),
        parser: SseParser::new(),
        chunk_state: ResponsesChunkState::default(),
        pending_events: VecDeque::new(),
        pending_error: None,
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

            if let Some(err) = state.pending_error.take() {
                state.stream_ended = true;
                return Some((Err(err), state));
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
                    let chunk_str = String::from_utf8_lossy(&bytes);
                    let chunk_redacted = crate::redact(&chunk_str);
                    log::trace!("responses body chunk: {chunk_redacted}");
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
                                state.pending_error = Some(ProviderError::Malformed(format!(
                                    "invalid SSE JSON: {e}"
                                )));
                                break;
                            }
                            Ok(json) => {
                                if let Some(m) = json
                                    .get("model")
                                    .or_else(|| json.get("response").and_then(|r| r.get("model")))
                                    .and_then(|v| v.as_str())
                                {
                                    state.model = Some(m.to_string());
                                }

                                match responses_chunk_to_events(&json, &mut state.chunk_state) {
                                    Err(err) => {
                                        state.stream_ended = true;
                                        state.pending_error = Some(err);
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

/// Owned configuration for opening (and reopening) a Responses API stream.
pub struct CodexStreamOpener {
    pub client: reqwest::Client,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    pub model: String,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub max_retries: u32,
    pub backoff_base: Duration,
}

impl CodexStreamOpener {
    /// POST the request, retrying the connection itself with backoff; returns the parsed event stream.
    pub async fn open(&self) -> Result<EventStream, ProviderError> {
        let response = self.post().await?;
        Ok(create_responses_event_stream(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
        ))
    }

    async fn post(&self) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut req_builder = self.client.post(&self.url).json(&self.body);

            for (k, v) in &self.headers {
                req_builder = req_builder.header(k, v);
            }

            match req_builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let status_u16 = status.as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    let redacted_body = crate::redact(&text);
                    log::trace!("responses error body: {redacted_body}");
                    log::warn!("codex request failed ({status_u16}): {redacted_body}");

                    if status_u16 == 401 || status_u16 == 403 {
                        return Err(ProviderError::Auth(format!("{status_u16}: {text}")));
                    }

                    let is_retryable = status_u16 == 429 || status_u16 >= 500;
                    if is_retryable && attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }

                    if status_u16 == 429 {
                        return Err(ProviderError::RateLimited(text));
                    }
                    return Err(ProviderError::Http {
                        status: status_u16,
                        message: text,
                    });
                }
                Err(err) => {
                    let err_str = err.to_string();
                    let redacted_err = crate::redact(&err_str);
                    log::warn!("codex request transport error: {redacted_err}");
                    if attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(ProviderError::Transport(err.to_string()));
                }
            }
        }
    }
}

fn is_committing(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::TextDelta { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallArgsDelta { .. }
            | StreamEvent::Usage(_)
            | StreamEvent::MessageEnd { .. }
    )
}

struct RetryState {
    opener: CodexStreamOpener,
    inner: Option<EventStream>,
    attempt: u32,
    max_retries: u32,
    committed: bool,
    done: bool,
}

/// Wrap `initial` so that a retryable failure before any output reopens the request.
pub fn retrying_responses_stream(opener: CodexStreamOpener, initial: EventStream) -> EventStream {
    let max_retries = opener.max_retries;
    let state = RetryState {
        opener,
        inner: Some(initial),
        attempt: 0,
        max_retries,
        committed: false,
        done: false,
    };

    let stream = futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.done {
                return None;
            }

            if state.inner.is_none() {
                match state.opener.open().await {
                    Ok(inner) => state.inner = Some(inner),
                    Err(err) => {
                        if err.is_retryable() && state.attempt < state.max_retries {
                            crate::retry::sleep_backoff(state.attempt, state.opener.backoff_base)
                                .await;
                            state.attempt += 1;
                            continue;
                        }
                        state.done = true;
                        return Some((Err(err), state));
                    }
                }
            }

            let next = match state.inner.as_mut() {
                Some(inner) => inner.next().await,
                None => return None,
            };

            match next {
                Some(Ok(event)) => {
                    if is_committing(&event) {
                        state.committed = true;
                    }
                    return Some((Ok(event), state));
                }
                Some(Err(err)) => {
                    if !state.committed && err.is_retryable() && state.attempt < state.max_retries {
                        let redacted = crate::redact(&err.to_string());
                        let attempt = state.attempt;
                        log::warn!(
                            "codex stream failed before output (attempt {attempt}), reopening: {redacted}"
                        );
                        crate::retry::sleep_backoff(state.attempt, state.opener.backoff_base).await;
                        state.attempt += 1;
                        state.inner = None;
                        continue;
                    }
                    state.done = true;
                    return Some((Err(err), state));
                }
                None => return None,
            }
        }
    });

    Box::pin(stream)
}
