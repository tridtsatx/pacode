//! Streaming decoder, stream opener, and retry wrapper for Devin API.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use pacode_types::{Effort, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage};
use serde_json::Value;

use super::connect::ConnectClient;
use super::wire::{
    AssignModelRequest, AssignModelResponse, GetChatMessageRequest, GetChatMessageResponse,
};
use crate::{CompletionRequest, EventStream, ProviderError};

/// Create an [`EventStream`] decoding Devin Connect JSON values into [`StreamEvent`]s.
pub fn create_devin_event_stream(
    inner: Pin<Box<dyn Stream<Item = Result<Value, ProviderError>> + Send>>,
    model: String,
) -> EventStream {
    struct StreamState {
        inner: Pin<Box<dyn Stream<Item = Result<Value, ProviderError>> + Send>>,
        model: String,
        pending_events: VecDeque<StreamEvent>,
        message_start_emitted: bool,
        message_end_emitted: bool,
        stream_ended: bool,
    }

    let initial = StreamState {
        inner,
        model,
        pending_events: VecDeque::new(),
        message_start_emitted: false,
        message_end_emitted: false,
        stream_ended: false,
    };

    let stream = futures::stream::unfold(initial, |mut state| async move {
        loop {
            if let Some(event) = state.pending_events.pop_front() {
                return Some((Ok(event), state));
            }

            if state.stream_ended {
                return None;
            }

            match state.inner.next().await {
                Some(Ok(value)) => {
                    let resp = match serde_json::from_value::<GetChatMessageResponse>(value) {
                        Ok(r) => r,
                        Err(e) => {
                            state.stream_ended = true;
                            return Some((
                                Err(ProviderError::Malformed(format!(
                                    "GetChatMessage failed to deserialize response: {e}"
                                ))),
                                state,
                            ));
                        }
                    };

                    let has_content = resp.delta_thinking.as_ref().is_some_and(|t| !t.is_empty())
                        || resp.delta_text.as_ref().is_some_and(|t| !t.is_empty());

                    if has_content && !state.message_start_emitted {
                        state.pending_events.push_back(StreamEvent::MessageStart {
                            model: Some(state.model.clone()),
                        });
                        state.message_start_emitted = true;
                    }

                    if let Some(thinking) = resp.delta_thinking
                        && !thinking.is_empty()
                    {
                        state
                            .pending_events
                            .push_back(StreamEvent::ReasoningDelta { text: thinking });
                    }

                    if let Some(text) = resp.delta_text
                        && !text.is_empty()
                    {
                        state
                            .pending_events
                            .push_back(StreamEvent::TextDelta { text });
                    }

                    if let Some(u) = resp.usage {
                        state.pending_events.push_back(StreamEvent::Usage(Usage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            reasoning_tokens: u.reasoning_tokens,
                            cache_read_tokens: u.cache_read_input_tokens,
                            cache_write_tokens: u.cache_creation_input_tokens,
                        }));
                    }

                    if let Some(sr) = resp.stop_reason {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }

                        let stop = match sr.as_str() {
                            "end_turn" | "stop" | "complete" => StopReason::EndTurn,
                            "tool_use" | "tool_calls" => StopReason::ToolUse,
                            "max_tokens" | "length" => StopReason::MaxTokens,
                            "content_filter" => StopReason::ContentFilter,
                            other => StopReason::Other(other.to_string()),
                        };
                        state
                            .pending_events
                            .push_back(StreamEvent::MessageEnd { stop });
                        state.message_end_emitted = true;
                    }
                }
                Some(Err(err)) => {
                    state.stream_ended = true;
                    return Some((Err(err), state));
                }
                None => {
                    state.stream_ended = true;
                    if !state.message_end_emitted {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        state.pending_events.push_back(StreamEvent::MessageEnd {
                            stop: StopReason::EndTurn,
                        });
                        state.message_end_emitted = true;
                    }
                }
            }
        }
    });

    Box::pin(stream)
}

/// Owned configuration for opening (and reopening) a Devin streaming completion.
pub struct DevinStreamOpener {
    pub connect_client: ConnectClient,
    pub req: CompletionRequest,
    pub defaults: ProviderDefaults,
    pub cfg: ProviderConfig,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub max_retries: u32,
    pub backoff_base: Duration,
}

impl DevinStreamOpener {
    /// Execute AssignModel then GetChatMessage, retrying the connection with backoff.
    pub async fn open(&self) -> Result<EventStream, ProviderError> {
        let mut attempt = 0;
        let timeout_secs = self.stream_idle_secs * self.effort.idle_timeout_factor();
        let idle_duration = Duration::from_secs(timeout_secs.max(1));
        let idle_timeout = if self.stream_idle_secs > 0 {
            Some((idle_duration, timeout_secs))
        } else {
            None
        };

        loop {
            let assign_req = AssignModelRequest::to_wire(&self.req.model);
            let assign_result = self
                .connect_client
                .unary::<_, AssignModelResponse>(
                    "/exa.api_server_pb.ApiServerService/AssignModel",
                    &assign_req,
                )
                .await;

            let assign_resp = match assign_result {
                Ok(resp) => resp,
                Err(err) => {
                    if err.is_retryable() && attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            };

            let jwt = assign_resp.assignment.assignment_jwt;
            let chat_req =
                GetChatMessageRequest::to_wire(&jwt, &self.req, &self.defaults, &self.cfg);

            let stream_result = self
                .connect_client
                .server_stream_with_idle_timeout(
                    "/exa.api_server_pb.ApiServerService/GetChatMessage",
                    &chat_req,
                    idle_timeout,
                )
                .await;

            match stream_result {
                Ok(inner_stream) => {
                    return Ok(create_devin_event_stream(
                        inner_stream,
                        self.req.model.clone(),
                    ));
                }
                Err(err) => {
                    if err.is_retryable() && attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }
}

fn is_committing(event: &StreamEvent) -> bool {
    match event {
        StreamEvent::TextDelta { .. }
        | StreamEvent::ReasoningDelta { .. }
        | StreamEvent::ToolCallStart { .. }
        | StreamEvent::ToolCallArgsDelta { .. }
        | StreamEvent::Usage(_)
        | StreamEvent::MessageEnd { .. } => true,
        StreamEvent::MessageStart { .. } => false,
    }
}

struct DevinRetryState {
    opener: DevinStreamOpener,
    inner: Option<EventStream>,
    attempt: u32,
    max_retries: u32,
    committed: bool,
    done: bool,
}

/// Wrap `initial` event stream so that a retryable error before any output reopens the request.
pub fn devin_retrying_stream(opener: DevinStreamOpener, initial: EventStream) -> EventStream {
    let max_retries = opener.max_retries;
    let state = DevinRetryState {
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
                            "devin stream failed before output (attempt {attempt}), reopening: {redacted}"
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
