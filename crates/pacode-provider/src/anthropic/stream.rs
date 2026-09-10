//! Streaming SSE decoder for the Anthropic Messages API.

use std::collections::VecDeque;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{CallId, Effort, StopReason, StreamEvent, Usage};
use serde_json::Value;

use super::types::map_tool_name_from_oauth;
use crate::sse::SseParser;
use crate::{EventStream, ProviderError};

/// Bookkeeping state across Anthropic SSE chunks.
#[derive(Default)]
pub struct AnthropicState {
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub message_start_emitted: bool,
    pub message_end_emitted: bool,
    pub stream_ended: bool,
}

/// Parse one Anthropic SSE JSON payload into stream events.
pub fn parse_anthropic_payload(
    json: &Value,
    state: &mut AnthropicState,
    is_oauth: bool,
) -> Result<Vec<StreamEvent>, ProviderError> {
    let mut events = Vec::new();

    let event_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "message_start" => {
            if let Some(msg) = json.get("message") {
                if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                    state.model = Some(m.to_string());
                }
                if let Some(usage) = msg.get("usage") {
                    if let Some(inp) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        state.input_tokens = inp;
                    }
                    if let Some(cache_read) = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        state.cache_read_tokens = cache_read;
                    }
                    if let Some(cache_write) = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        state.cache_write_tokens = cache_write;
                    }
                }
            }
        }
        "content_block_start" => {
            let index = json
                .get("index")
                .and_then(|v| v.as_u64())
                .map(|i| i as u32)
                .unwrap_or(0);
            if let Some(cb) = json.get("content_block") {
                match cb.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        if let Some(text) = cb.get("text").and_then(|v| v.as_str())
                            && !text.is_empty()
                        {
                            events.push(StreamEvent::TextDelta {
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("thinking") => {
                        if let Some(thinking) = cb.get("thinking").and_then(|v| v.as_str())
                            && !thinking.is_empty()
                        {
                            events.push(StreamEvent::ReasoningDelta {
                                text: thinking.to_string(),
                            });
                        }
                    }
                    Some("tool_use") => {
                        let id = cb
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(CallId::new)
                            .unwrap_or_else(CallId::generate);
                        let raw_name = cb.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                        let name = if is_oauth {
                            map_tool_name_from_oauth(raw_name)
                        } else {
                            raw_name.to_string()
                        };
                        events.push(StreamEvent::ToolCallStart { index, id, name });
                    }
                    _ => {}
                }
            }
        }
        "content_block_delta" => {
            let index = json
                .get("index")
                .and_then(|v| v.as_u64())
                .map(|i| i as u32)
                .unwrap_or(0);
            if let Some(delta) = json.get("delta") {
                match delta.get("type").and_then(|v| v.as_str()) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str())
                            && !text.is_empty()
                        {
                            events.push(StreamEvent::TextDelta {
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(|v| v.as_str())
                            && !thinking.is_empty()
                        {
                            events.push(StreamEvent::ReasoningDelta {
                                text: thinking.to_string(),
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str())
                            && !partial.is_empty()
                        {
                            events.push(StreamEvent::ToolCallArgsDelta {
                                index,
                                delta: partial.to_string(),
                            });
                        }
                    }
                    Some("signature_delta") => {}
                    _ => {}
                }
            }
        }
        "content_block_stop" => {}
        "message_delta" => {
            if let Some(usage) = json.get("usage")
                && let Some(out) = usage.get("output_tokens").and_then(|v| v.as_u64())
            {
                state.output_tokens = out;
            }
            events.push(StreamEvent::Usage(Usage {
                input_tokens: state.input_tokens,
                output_tokens: state.output_tokens,
                reasoning_tokens: state.reasoning_tokens,
                cache_read_tokens: state.cache_read_tokens,
                cache_write_tokens: state.cache_write_tokens,
            }));
            if let Some(delta) = json.get("delta")
                && let Some(sr) = delta.get("stop_reason").and_then(|v| v.as_str())
            {
                let stop = match sr {
                    "end_turn" => StopReason::EndTurn,
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    "stop_sequence" => StopReason::EndTurn,
                    other => StopReason::Other(other.to_string()),
                };
                events.push(StreamEvent::MessageEnd { stop });
                state.message_end_emitted = true;
            }
        }
        "message_stop" => {
            if !state.message_end_emitted {
                events.push(StreamEvent::MessageEnd {
                    stop: StopReason::EndTurn,
                });
                state.message_end_emitted = true;
            }
            state.stream_ended = true;
        }
        "ping" => {}
        "error" => {
            let err_obj = json.get("error").unwrap_or(json);
            let err_type = err_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let message = err_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Anthropic API error");
            let err = match err_type {
                "authentication_error" => ProviderError::Auth(message.to_string()),
                "rate_limit_error" => ProviderError::RateLimited(message.to_string()),
                "overloaded_error" => ProviderError::RateLimited(format!("overloaded: {message}")),
                "invalid_request_error" => ProviderError::Http {
                    status: 400,
                    message: message.to_string(),
                },
                "api_error" => ProviderError::Http {
                    status: 500,
                    message: message.to_string(),
                },
                _ => ProviderError::Malformed(message.to_string()),
            };
            return Err(err);
        }
        _ => {}
    }

    Ok(events)
}

pub(crate) fn create_anthropic_event_stream(
    response: reqwest::Response,
    req_model: String,
    stream_idle_secs: u64,
    effort: Effort,
    is_oauth: bool,
) -> EventStream {
    let timeout_secs = stream_idle_secs * effort.idle_timeout_factor();
    let idle_duration = Duration::from_secs(timeout_secs.max(1));

    struct StreamContext<S> {
        byte_stream: S,
        parser: SseParser,
        state: AnthropicState,
        pending_events: VecDeque<StreamEvent>,
        pending_error: Option<ProviderError>,
        idle_duration: Duration,
        timeout_secs: u64,
        is_oauth: bool,
    }

    let initial = StreamContext {
        byte_stream: Box::pin(response.bytes_stream()),
        parser: SseParser::new(),
        state: AnthropicState {
            model: Some(req_model),
            ..AnthropicState::default()
        },
        pending_events: VecDeque::new(),
        pending_error: None,
        idle_duration,
        timeout_secs,
        is_oauth,
    };

    let stream = futures::stream::unfold(initial, |mut ctx| async move {
        loop {
            if let Some(event) = ctx.pending_events.pop_front() {
                return Some((Ok(event), ctx));
            }

            if let Some(err) = ctx.pending_error.take() {
                ctx.state.stream_ended = true;
                return Some((Err(err), ctx));
            }

            if ctx.state.stream_ended {
                return None;
            }

            let next_chunk = tokio::time::timeout(ctx.idle_duration, ctx.byte_stream.next()).await;

            match next_chunk {
                Err(_) => {
                    ctx.state.stream_ended = true;
                    return Some((Err(ProviderError::IdleTimeout(ctx.timeout_secs)), ctx));
                }
                Ok(Some(Ok(bytes))) => {
                    let chunk_str = String::from_utf8_lossy(&bytes);
                    let chunk_redacted = crate::redact(&chunk_str);
                    log::trace!("anthropic response chunk: {chunk_redacted}");
                    let payloads = ctx.parser.feed(bytes.as_ref());
                    for payload in payloads {
                        let json = match serde_json::from_str::<Value>(&payload) {
                            Ok(j) => j,
                            Err(e) => {
                                ctx.state.stream_ended = true;
                                ctx.pending_error = Some(ProviderError::Malformed(format!(
                                    "invalid Anthropic SSE JSON: {e}"
                                )));
                                break;
                            }
                        };

                        match parse_anthropic_payload(&json, &mut ctx.state, ctx.is_oauth) {
                            Err(err) => {
                                ctx.state.stream_ended = true;
                                ctx.pending_error = Some(err);
                                break;
                            }
                            Ok(events) => {
                                for event in events {
                                    if !ctx.state.message_start_emitted
                                        && matches!(
                                            event,
                                            StreamEvent::TextDelta { .. }
                                                | StreamEvent::ReasoningDelta { .. }
                                                | StreamEvent::ToolCallStart { .. }
                                                | StreamEvent::ToolCallArgsDelta { .. }
                                        )
                                    {
                                        ctx.pending_events.push_back(StreamEvent::MessageStart {
                                            model: ctx.state.model.clone(),
                                        });
                                        ctx.state.message_start_emitted = true;
                                    }
                                    ctx.pending_events.push_back(event);
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(err))) => {
                    ctx.state.stream_ended = true;
                    return Some((Err(ProviderError::Transport(err.to_string())), ctx));
                }
                Ok(None) => {
                    ctx.state.stream_ended = true;
                    if !ctx.state.message_end_emitted {
                        ctx.pending_events.push_back(StreamEvent::MessageEnd {
                            stop: StopReason::EndTurn,
                        });
                        ctx.state.message_end_emitted = true;
                    }
                }
            }
        }
    });

    Box::pin(stream)
}
