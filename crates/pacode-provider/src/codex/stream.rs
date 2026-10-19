//! Event stream construction and retry policy for OpenAI Responses API.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use pacode_types::{Effort, StopReason, StreamEvent};
use serde_json::Value;

use super::events::{ResponsesChunkState, responses_chunk_to_events};
use crate::governor::{ProviderGovernor, governor_for, hold_permit};
use crate::post::{HttpPost, post_with_retries};
use crate::retry::{OpenedStream, RetryAfterSignal, RetryBudget, StreamTransport};
use crate::sse::SseParser;
use crate::{EventStream, ProviderError};

/// Create an [`EventStream`] decoding Responses API SSE chunks into pacode [`StreamEvent`]s.
pub fn create_responses_event_stream(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
) -> EventStream {
    create_responses_event_stream_with_retry_after(
        response,
        req_model,
        stream_idle_secs,
        effort,
        RetryAfterSignal::default(),
    )
}

pub(crate) fn create_responses_event_stream_with_retry_after(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
    retry_after: RetryAfterSignal,
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
        retry_after: RetryAfterSignal,
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

            let next_chunk =
                tokio::time::timeout(state.idle_duration, state.byte_stream.next()).await;

            match next_chunk {
                Err(_) => {
                    state.stream_ended = true;
                    return Some((Err(ProviderError::IdleTimeout(state.timeout_secs)), state));
                }
                Ok(Some(Ok(bytes))) => {
                    // The lossy copy + redact scan run per network chunk — only
                    // when `trace` is actually on (off in release builds).
                    if log::log_enabled!(log::Level::Trace) {
                        let chunk_str = String::from_utf8_lossy(&bytes);
                        let chunk_redacted = crate::redact(&chunk_str);
                        log::trace!("responses body chunk: {chunk_redacted}");
                    }
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
                                    "invalid SSE JSON: {e}: {}",
                                    crate::error_body_message(&payload)
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
                                        if matches!(&err, ProviderError::RateLimited(_))
                                            && let Some(delay) = json
                                                .get("error")
                                                .and_then(crate::sse::retry_after_from_error)
                                        {
                                            state.retry_after.set(delay);
                                        }
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
#[derive(Clone)]
pub struct CodexStreamOpener {
    pub client: reqwest::Client,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Serialised once (see `request_body`): each retry is a refcount bump instead of
    /// a DOM re-serialization; released after the first committing event.
    pub body: Bytes,
    pub model: String,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub backoff_base: Duration,
}

impl CodexStreamOpener {
    /// POST the request, retrying the connection itself with backoff; returns the parsed event stream.
    pub async fn open(&self, mut budget: RetryBudget) -> Result<EventStream, ProviderError> {
        let governor = self.governor();
        let opened = self.open_once(&governor, &mut budget).await?;
        Ok(crate::retry::retrying_transport_stream(
            self.clone(),
            governor,
            opened,
            budget,
        ))
    }
}

impl StreamTransport for CodexStreamOpener {
    fn governor(&self) -> Arc<ProviderGovernor> {
        governor_for(&self.url, None, &self.headers)
    }

    fn backoff_base(&self) -> Duration {
        self.backoff_base
    }

    async fn open_once(
        &self,
        governor: &Arc<ProviderGovernor>,
        budget: &mut RetryBudget,
    ) -> Result<OpenedStream, ProviderError> {
        let post = HttpPost {
            label: "codex",
            client: &self.client,
            url: &self.url,
            api_key: None,
            headers: &self.headers,
            body: &self.body,
            stream_idle_secs: self.stream_idle_secs,
            effort: self.effort,
            backoff_base: self.backoff_base,
        };
        let (response, permit) = post_with_retries(&post, governor, budget).await?;
        let retry_after = RetryAfterSignal::default();
        let inner = create_responses_event_stream_with_retry_after(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
            retry_after.clone(),
        );
        // The slot is freed at `MessageEnd`, error or drop, and the first event
        // credits the account's AIMD increase.
        Ok(OpenedStream {
            stream: hold_permit(inner, permit),
            retry_after,
        })
    }

    fn release_body(&mut self) {
        if !self.body.is_empty() {
            self.body = Bytes::new();
        }
    }
}
