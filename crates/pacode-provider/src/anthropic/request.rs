//! Request body construction for the Anthropic Messages API.

use pacode_types::{ContentBlock, Effort, Role};
use serde_json::Value;

use super::types::{CLAUDE_CODE_IDENTITY, DEFAULT_MAX_OUTPUT_TOKENS, map_tool_name_for_oauth};
use crate::CompletionRequest;

/// Build the JSON payload for an Anthropic Messages API request.
pub fn build_body(
    req: &CompletionRequest,
    is_oauth: bool,
    reasoning_enabled: bool,
    default_effort: Effort,
    extra_body: Option<&Value>,
) -> Value {
    let mut body = serde_json::Map::new();

    body.insert("model".to_string(), serde_json::json!(req.model));
    let max_tokens = req.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    body.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
    body.insert("stream".to_string(), serde_json::json!(true));

    let mut system_blocks = Vec::new();
    if is_oauth {
        system_blocks.push(serde_json::json!({
            "type": "text",
            "text": CLAUDE_CODE_IDENTITY,
        }));
    }
    if !req.system_static.is_empty() {
        system_blocks.push(serde_json::json!({
            "type": "text",
            "text": req.system_static,
        }));
    }
    if !req.system_dynamic.is_empty() {
        system_blocks.push(serde_json::json!({
            "type": "text",
            "text": req.system_dynamic,
        }));
    }
    if !system_blocks.is_empty() {
        body.insert("system".to_string(), Value::Array(system_blocks));
    }

    let messages = build_messages(&req.messages, is_oauth);
    body.insert("messages".to_string(), Value::Array(messages));

    if !req.tools.is_empty() {
        let tools_val: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let name = if is_oauth {
                    map_tool_name_for_oauth(&t.name)
                } else {
                    t.name.clone()
                };
                serde_json::json!({
                    "name": name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        body.insert("tools".to_string(), Value::Array(tools_val));
    }

    if reasoning_enabled {
        let effort = req.effort.unwrap_or(default_effort);
        let budget = match effort {
            Effort::Low => 1024,
            Effort::Medium => 4096,
            Effort::High => 8192,
            Effort::XHigh | Effort::Max => 16384,
        };
        let clamped_budget = budget.min(max_tokens.saturating_sub(1)).max(1024);
        body.insert(
            "thinking".to_string(),
            serde_json::json!({
                "type": "enabled",
                "budget_tokens": clamped_budget,
            }),
        );
    }

    let mut body_val = Value::Object(body);
    if let Some(extra) = extra_body
        && extra.is_object()
    {
        deep_merge(&mut body_val, extra);
    }
    body_val
}

fn build_messages(messages: &[pacode_types::Message], is_oauth: bool) -> Vec<Value> {
    let mut raw_messages: Vec<(&str, Vec<Value>)> = Vec::new();
    for msg in messages {
        match msg.role {
            Role::User => {
                let mut content = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                content.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::Reasoning { .. } => {}
                        ContentBlock::ToolUse { id, name, input } => {
                            let tool_name = if is_oauth {
                                map_tool_name_for_oauth(name)
                            } else {
                                name.clone()
                            };
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": id.as_str(),
                                "name": tool_name,
                                "input": input,
                            }));
                        }
                        ContentBlock::ToolResult {
                            call_id,
                            content: res,
                            is_error,
                        } => {
                            content.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": call_id.as_str(),
                                "content": res,
                                "is_error": is_error,
                            }));
                        }
                    }
                }
                if !content.is_empty() {
                    raw_messages.push(("user", content));
                }
            }
            Role::Assistant => {
                let mut content = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                content.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::Reasoning { text, signature } => {
                            if let Some(sig) = signature {
                                content.push(serde_json::json!({
                                    "type": "thinking",
                                    "thinking": text,
                                    "signature": sig,
                                }));
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let tool_name = if is_oauth {
                                map_tool_name_for_oauth(name)
                            } else {
                                name.clone()
                            };
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": id.as_str(),
                                "name": tool_name,
                                "input": input,
                            }));
                        }
                        ContentBlock::ToolResult {
                            call_id,
                            content: res,
                            is_error,
                        } => {
                            content.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": call_id.as_str(),
                                "content": res,
                                "is_error": is_error,
                            }));
                        }
                    }
                }
                if !content.is_empty() {
                    raw_messages.push(("assistant", content));
                }
            }
            Role::Tool => {
                let mut content = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::ToolResult {
                            call_id,
                            content: res,
                            is_error,
                        } => {
                            content.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": call_id.as_str(),
                                "content": res,
                                "is_error": is_error,
                            }));
                        }
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                content.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::Reasoning { .. } => {}
                        ContentBlock::ToolUse { id, name, input } => {
                            let tool_name = if is_oauth {
                                map_tool_name_for_oauth(name)
                            } else {
                                name.clone()
                            };
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": id.as_str(),
                                "name": tool_name,
                                "input": input,
                            }));
                        }
                    }
                }
                if !content.is_empty() {
                    raw_messages.push(("user", content));
                }
            }
            Role::System => {
                let text = msg.text();
                if !text.is_empty() {
                    raw_messages.push((
                        "user",
                        vec![serde_json::json!({
                            "type": "text",
                            "text": text,
                        })],
                    ));
                }
            }
        }
    }

    let mut merged: Vec<(&str, Vec<Value>)> = Vec::new();
    for (role, content) in raw_messages {
        if let Some((last_role, last_content)) = merged.last_mut()
            && *last_role == role
        {
            last_content.extend(content);
            continue;
        }
        merged.push((role, content));
    }

    if merged.last().is_some_and(|(role, _)| *role == "assistant") {
        merged.push((
            "user",
            vec![serde_json::json!({
                "type": "text",
                "text": "Continue.",
            })],
        ));
    }

    merged
        .into_iter()
        .map(|(role, content)| {
            serde_json::json!({
                "role": role,
                "content": content,
            })
        })
        .collect()
}

pub fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, val) in source_map {
                match target_map.get_mut(key) {
                    Some(target_val) => deep_merge(target_val, val),
                    None => {
                        target_map.insert(key.clone(), val.clone());
                    }
                }
            }
        }
        (target, source) => {
            *target = source.clone();
        }
    }
}
