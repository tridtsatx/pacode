//! Responses SSE event parsing and mapping to pacode StreamEvents.

use std::collections::{HashMap, HashSet};

use pacode_types::{CallId, StopReason, StreamEvent, Usage};
use serde_json::Value;

use crate::ProviderError;

/// Tracking state across Responses SSE chunks for one stream.
#[derive(Default)]
pub struct ResponsesChunkState {
    pub(crate) next_tool_index: u32,
    pub(crate) started_tools: HashMap<String, u32>,
    pub(crate) completed_tools: HashSet<String>,
    pub(crate) any_tool_call: bool,
    pub(crate) message_end_emitted: bool,
}

impl ResponsesChunkState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn any_tool_call(&self) -> bool {
        self.any_tool_call
    }

    pub fn message_end_emitted(&self) -> bool {
        self.message_end_emitted
    }
}

/// Convert an in-stream Responses error JSON payload into a [`ProviderError`].
pub fn extract_responses_error(val: &Value) -> ProviderError {
    let err_obj = val
        .get("error")
        .or_else(|| val.get("response").and_then(|r| r.get("error")))
        .unwrap_or(val);

    let message = err_obj
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| val.get("message").and_then(|v| v.as_str()))
        .unwrap_or("unknown error")
        .to_string();

    let code_opt = err_obj.get("code").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
    });

    let code_str = err_obj
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let lower_msg = message.to_ascii_lowercase();

    if code_opt == Some(401)
        || code_opt == Some(403)
        || code_str.contains("auth")
        || code_str.contains("invalid_api_key")
        || lower_msg.contains("unauthorized")
        || lower_msg.contains("authentication")
    {
        ProviderError::Auth(message)
    } else if code_opt == Some(429)
        || code_str.contains("rate_limit")
        || lower_msg.contains("rate limit")
    {
        ProviderError::RateLimited(message)
    } else if let Some(code) = code_opt
        && (400..=599).contains(&code)
    {
        ProviderError::Http {
            status: code as u16,
            message,
        }
    } else {
        ProviderError::Malformed(message)
    }
}

/// Parse one Responses SSE JSON payload into [`StreamEvent`]s.
pub fn responses_chunk_to_events(
    chunk: &Value,
    state: &mut ResponsesChunkState,
) -> Result<Vec<StreamEvent>, ProviderError> {
    let kind = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");

    if kind == "response.failed" || kind == "response.error" || kind == "error" {
        return Err(extract_responses_error(chunk));
    }
    if let Some(err_val) = chunk.get("error") {
        return Err(extract_responses_error(err_val));
    }

    let mut events = Vec::new();

    match kind {
        "response.output_text.delta" => {
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str())
                && !delta.is_empty()
            {
                events.push(StreamEvent::TextDelta {
                    text: delta.to_string(),
                });
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning.delta" => {
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str())
                && !delta.is_empty()
            {
                events.push(StreamEvent::ReasoningDelta {
                    text: delta.to_string(),
                });
            }
        }
        "response.output_item.added" => {
            handle_output_item_added(chunk, state, &mut events);
        }
        "response.function_call_arguments.delta" => {
            handle_function_call_arguments_delta(chunk, state, &mut events);
        }
        "response.function_call_arguments.done" => {
            handle_function_call_arguments_done(chunk, state, &mut events);
        }
        "response.output_item.done" => {
            handle_output_item_done(chunk, state, &mut events);
        }
        "response.completed" | "response.incomplete" => {
            handle_response_completed(chunk, state, &mut events);
        }
        _ => {}
    }

    Ok(events)
}

fn handle_output_item_added(
    chunk: &Value,
    state: &mut ResponsesChunkState,
    events: &mut Vec<StreamEvent>,
) {
    if let Some(item) = chunk.get("item")
        && matches!(
            item.get("type").and_then(|v| v.as_str()),
            Some("function_call") | Some("custom_tool_call")
        )
    {
        let item_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let call_id = item
            .get("call_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .or_else(|| item_id.clone())
            .unwrap_or_else(|| format!("call_{}", state.next_tool_index));
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let args = item
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !name.is_empty() {
            let index = state.next_tool_index;
            state.next_tool_index += 1;
            state.any_tool_call = true;
            if let Some(id) = item_id {
                state.started_tools.insert(id, index);
            }
            state.started_tools.insert(call_id.clone(), index);

            events.push(StreamEvent::ToolCallStart {
                index,
                id: CallId::new(call_id),
                name,
            });
            if !args.is_empty() {
                events.push(StreamEvent::ToolCallArgsDelta { index, delta: args });
            }
        }
    }
}

fn handle_function_call_arguments_delta(
    chunk: &Value,
    state: &ResponsesChunkState,
    events: &mut Vec<StreamEvent>,
) {
    let delta = chunk.get("delta").and_then(|v| v.as_str());
    let item_id = chunk.get("item_id").and_then(|v| v.as_str());
    let call_id = chunk.get("call_id").and_then(|v| v.as_str());
    let key = item_id.or(call_id);
    if let Some(d) = delta
        && let Some(k) = key
        && let Some(&index) = state.started_tools.get(k)
    {
        events.push(StreamEvent::ToolCallArgsDelta {
            index,
            delta: d.to_string(),
        });
    }
}

fn handle_function_call_arguments_done(
    chunk: &Value,
    state: &mut ResponsesChunkState,
    events: &mut Vec<StreamEvent>,
) {
    let item_id = chunk.get("item_id").and_then(|v| v.as_str());
    let call_id = chunk.get("call_id").and_then(|v| v.as_str());
    let key = item_id.or(call_id);
    let name = chunk.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = chunk
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if let Some(k) = key {
        state.completed_tools.insert(k.to_string());
        if !state.started_tools.contains_key(k) && !name.is_empty() {
            let index = state.next_tool_index;
            state.next_tool_index += 1;
            state.any_tool_call = true;
            let cid = call_id.unwrap_or(k);
            events.push(StreamEvent::ToolCallStart {
                index,
                id: CallId::new(cid),
                name: name.to_string(),
            });
            if !args.is_empty() {
                events.push(StreamEvent::ToolCallArgsDelta {
                    index,
                    delta: args.to_string(),
                });
            }
        }
    }
}

fn handle_output_item_done(
    chunk: &Value,
    state: &mut ResponsesChunkState,
    events: &mut Vec<StreamEvent>,
) {
    if let Some(item) = chunk.get("item")
        && matches!(
            item.get("type").and_then(|v| v.as_str()),
            Some("function_call") | Some("custom_tool_call")
        )
    {
        let item_id = item.get("id").and_then(|v| v.as_str());
        let call_id = item.get("call_id").and_then(|v| v.as_str());
        let key = item_id.or(call_id);
        let is_already_handled = key.is_some_and(|k| {
            state.completed_tools.contains(k) || state.started_tools.contains_key(k)
        });

        if !is_already_handled {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
            let cid = call_id.or(item_id).unwrap_or("call_0");
            let index = state.next_tool_index;
            state.next_tool_index += 1;
            state.any_tool_call = true;
            if let Some(k) = key {
                state.completed_tools.insert(k.to_string());
            }

            events.push(StreamEvent::ToolCallStart {
                index,
                id: CallId::new(cid),
                name: name.to_string(),
            });
            events.push(StreamEvent::ToolCallArgsDelta {
                index,
                delta: if args.is_empty() {
                    "{}".to_string()
                } else {
                    args.to_string()
                },
            });
        }
    }
}

fn handle_response_completed(
    chunk: &Value,
    state: &mut ResponsesChunkState,
    events: &mut Vec<StreamEvent>,
) {
    let resp = chunk.get("response");
    if let Some(usage_val) = resp.and_then(|r| r.get("usage")).filter(|v| v.is_object()) {
        let input_tokens = usage_val
            .get("input_tokens")
            .or_else(|| usage_val.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage_val
            .get("output_tokens")
            .or_else(|| usage_val.get("completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let reasoning_tokens = usage_val
            .get("output_tokens_details")
            .or_else(|| usage_val.get("completion_tokens_details"))
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read_tokens = usage_val
            .get("input_tokens_details")
            .or_else(|| usage_val.get("prompt_tokens_details"))
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

    let stop = if state.any_tool_call {
        StopReason::ToolUse
    } else {
        let incomplete_reason = resp
            .and_then(|r| r.get("incomplete_details"))
            .and_then(|d| d.get("reason"))
            .and_then(|v| v.as_str());

        match incomplete_reason {
            Some("max_output_tokens" | "length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::ContentFilter,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::EndTurn,
        }
    };

    events.push(StreamEvent::MessageEnd { stop });
    state.message_end_emitted = true;
}
