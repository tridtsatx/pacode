//! Streaming decoder, stream opener, and retry wrapper for Devin API.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use pacode_types::{
    CallId, Effort, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage,
};

use super::connect::ConnectClient;
use super::tool_parser::InlineToolCallParser;
use super::wire::{decode_get_chat_message_response, encode_get_chat_message_request};
use crate::{CompletionRequest, EventStream, ProviderError};

/// Create an [`EventStream`] decoding Devin protobuf frames into [`StreamEvent`]s.
pub fn create_devin_event_stream(
    inner: Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>,
    model: String,
) -> EventStream {
    struct StreamState {
        inner: Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>,
        model: String,
        tool_parser: InlineToolCallParser,
        pending_events: VecDeque<StreamEvent>,
        message_start_emitted: bool,
        message_end_emitted: bool,
        stream_ended: bool,
        /// Tool calls the server delivered structurally, used to number them.
        structured_tool_calls: u32,
        /// Index of the structured tool call whose arguments are still streaming.
        active_structured_call: Option<u32>,
        thinking_kind: Option<String>,
    }

    let initial = StreamState {
        inner,
        model,
        tool_parser: InlineToolCallParser::new(),
        pending_events: VecDeque::new(),
        message_start_emitted: false,
        message_end_emitted: false,
        stream_ended: false,
        structured_tool_calls: 0,
        active_structured_call: None,
        thinking_kind: None,
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
                Some(Ok(bytes)) => {
                    let resp = match decode_get_chat_message_response(&bytes) {
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

                    if let Some(m) = resp.model_name
                        && !m.is_empty()
                    {
                        state.model = m;
                    }

                    if let Some(kind) = resp.thinking_kind {
                        state.thinking_kind = Some(kind);
                    }

                    if let Some(thinking) = resp.delta_thinking
                        && !thinking.is_empty()
                    {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        state
                            .pending_events
                            .push_back(StreamEvent::ReasoningDelta { text: thinking });
                    }

                    if let Some(sig) = resp.thinking_signature {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        state
                            .pending_events
                            .push_back(StreamEvent::ReasoningSignature {
                                signature: sig,
                                kind: state.thinking_kind.clone(),
                            });
                    }

                    if let Some(call) = resp.tool_call {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        // A frame naming a tool opens a call; the frames after it carry
                        // the arguments JSON in chunks, with no name of their own.
                        if !call.name.is_empty() {
                            let index = state.structured_tool_calls;
                            state.structured_tool_calls += 1;
                            state.active_structured_call = Some(index);
                            // The server matches tool results by the id it issued, so
                            // keep it whenever it sends one.
                            let id = if call.call_id.is_empty() {
                                CallId::generate()
                            } else {
                                CallId::from(call.call_id.as_str())
                            };
                            state.pending_events.push_back(StreamEvent::ToolCallStart {
                                index,
                                id,
                                name: call.name,
                            });
                        }
                        if !call.arguments_json.is_empty() {
                            match state.active_structured_call {
                                Some(index) => {
                                    state.pending_events.push_back(
                                        StreamEvent::ToolCallArgsDelta {
                                            index,
                                            delta: call.arguments_json,
                                        },
                                    );
                                }
                                None => {
                                    log::warn!(
                                        "devin sent tool call arguments before any tool call started; dropping {} bytes",
                                        call.arguments_json.len()
                                    );
                                }
                            }
                        }
                    }

                    if let Some(text) = resp.delta_text
                        && !text.is_empty()
                    {
                        let parsed_events = state.tool_parser.feed(&text);
                        for event in parsed_events {
                            if !state.message_start_emitted {
                                state.pending_events.push_back(StreamEvent::MessageStart {
                                    model: Some(state.model.clone()),
                                });
                                state.message_start_emitted = true;
                            }
                            state.pending_events.push_back(event);
                        }
                    }

                    if let Some(u) = resp.usage {
                        state.pending_events.push_back(StreamEvent::Usage(Usage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            reasoning_tokens: 0,
                            cache_read_tokens: u.cached_input_tokens,
                            cache_write_tokens: 0,
                        }));
                    }

                    if let Some(code) = resp.stop_reason {
                        let flushed = state.tool_parser.flush();
                        for event in flushed {
                            if !state.message_start_emitted {
                                state.pending_events.push_back(StreamEvent::MessageStart {
                                    model: Some(state.model.clone()),
                                });
                                state.message_start_emitted = true;
                            }
                            state.pending_events.push_back(event);
                        }

                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }

                        let stop = if state.tool_parser.has_tool_calls()
                            || state.structured_tool_calls > 0
                            || code == 4
                            || code == 10
                        {
                            StopReason::ToolUse
                        } else {
                            match code {
                                1 | 2 => StopReason::EndTurn,
                                3 => StopReason::MaxTokens,
                                other => StopReason::Other(format!("{other}")),
                            }
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
                    let flushed = state.tool_parser.flush();
                    for event in flushed {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        state.pending_events.push_back(event);
                    }

                    if !state.message_end_emitted {
                        if !state.message_start_emitted {
                            state.pending_events.push_back(StreamEvent::MessageStart {
                                model: Some(state.model.clone()),
                            });
                            state.message_start_emitted = true;
                        }
                        let stop = if state.tool_parser.has_tool_calls() {
                            StopReason::ToolUse
                        } else {
                            StopReason::EndTurn
                        };
                        state
                            .pending_events
                            .push_back(StreamEvent::MessageEnd { stop });
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
    pub session_token: String,
    pub req: CompletionRequest,
    pub defaults: ProviderDefaults,
    pub cfg: ProviderConfig,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub assignment_jwt: Option<String>,
}

impl DevinStreamOpener {
    /// Execute GetChatMessage directly, selecting the model via request field 21.
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
            let req_bytes = encode_get_chat_message_request(
                &self.session_token,
                &self.req,
                &self.cfg,
                self.assignment_jwt.as_deref(),
            );

            let stream_result = self
                .connect_client
                .server_stream_with_idle_timeout(
                    "/exa.api_server_pb.ApiServerService/GetChatMessage",
                    &req_bytes,
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
        | StreamEvent::ReasoningSignature { .. }
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
