//! Streaming decoder, stream opener, and retry wrapper for Devin API.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use pacode_types::{
    CallId, Effort, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage,
};

use super::connect::ConnectClient;
use super::tool_parser::InlineToolCallParser;
use super::wire::{decode_get_chat_message_response, encode_get_chat_message_request};
use crate::governor::{ProviderGovernor, governor_for, hold_permit};
use crate::retry::{
    MIN_429_COOLDOWN, OpenedStream, RetryAfterSignal, RetryBudget, StreamTransport, retry_delay,
    sleep_delay,
};
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
#[derive(Clone)]
pub struct DevinStreamOpener {
    pub connect_client: ConnectClient,
    pub session_token: String,
    pub req: CompletionRequest,
    pub defaults: ProviderDefaults,
    pub cfg: ProviderConfig,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub backoff_base: Duration,
    pub assignment_jwt: Option<String>,
}

impl DevinStreamOpener {
    /// Execute GetChatMessage directly, selecting the model via request field 21.
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

impl StreamTransport for DevinStreamOpener {
    fn governor(&self) -> Arc<ProviderGovernor> {
        governor_for(
            self.connect_client.api_server_url(),
            Some(self.connect_client.api_key()),
            &[],
        )
    }

    fn backoff_base(&self) -> Duration {
        self.backoff_base
    }

    async fn open_once(
        &self,
        governor: &Arc<ProviderGovernor>,
        budget: &mut RetryBudget,
    ) -> Result<OpenedStream, ProviderError> {
        let timeout_secs = self
            .stream_idle_secs
            .saturating_mul(self.effort.idle_timeout_factor());
        let idle_timeout = (self.stream_idle_secs > 0)
            .then_some((Duration::from_secs(timeout_secs.max(1)), timeout_secs));

        // The request is identical across retries — encode the proto once and
        // resend the same bytes instead of re-walking the message history.
        let req_bytes = encode_get_chat_message_request(
            &self.session_token,
            &self.req,
            &self.cfg,
            self.assignment_jwt.as_deref(),
        );
        let queued_bytes = req_bytes.len() as u64;

        loop {
            let permit = governor.acquire(queued_bytes).await?;
            match self
                .connect_client
                .server_stream(
                    "/exa.api_server_pb.ApiServerService/GetChatMessage",
                    &req_bytes,
                    idle_timeout,
                )
                .await
            {
                Ok(inner_stream) => {
                    let retry_after = RetryAfterSignal::default();
                    let inner = create_devin_event_stream(inner_stream, self.req.model.clone());
                    return Ok(OpenedStream {
                        stream: hold_permit(inner, permit),
                        retry_after,
                    });
                }
                Err(failure) => {
                    let delay = retry_delay(self.backoff_base, budget.used, failure.retry_after);
                    if failure.retry_after.is_some()
                        || matches!(&failure.error, ProviderError::RateLimited(_))
                    {
                        governor.start_cooldown(delay.max(MIN_429_COOLDOWN));
                    }
                    drop(permit);
                    if failure.error.is_retryable() && budget.reserve() {
                        sleep_delay(delay).await;
                        continue;
                    }
                    return Err(failure.error);
                }
            }
        }
    }
}
