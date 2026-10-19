//! Request body construction for the OpenAI Responses API.
//!
//! Images: user messages carry `input_image` parts (`image_url` = `data:` URL, bytes
//! loaded lazily from the image store) after the `input_text` part; models without
//! vision get text placeholders. Images returned by tools go into one user follow-up
//! message after the whole run of `function_call_output` items (a plain-string `output`
//! cannot hold pixels).

use pacode_types::{ContentBlock, Effort, ImageSource, ProviderConfig, ProviderDefaults, Role};
use serde_json::Value;

use crate::CompletionRequest;

pub const REASONING_KEYWORDS: &[&str] = &[
    "codex",
    "gemini",
    "o1",
    "o3",
    "o4",
    "gpt-5",
    "deepseek-r",
    "qwq",
    "thinking",
    "reason",
    "glm-5",
    "kimi-k2",
    "claude",
    "minimax",
];

/// Build the JSON body for `req` in OpenAI Responses API format.
pub fn build_responses_body(
    cfg: &ProviderConfig,
    defaults: &ProviderDefaults,
    is_oauth: bool,
    req: &CompletionRequest,
) -> Value {
    let mut instructions = if req.system_dynamic.is_empty() {
        req.system_static.clone()
    } else if req.system_static.is_empty() {
        req.system_dynamic.clone()
    } else {
        format!("{}\n\n{}", req.system_static, req.system_dynamic)
    };

    for msg in &req.messages {
        if msg.role == Role::System {
            let text = msg.text();
            if !text.is_empty() {
                if instructions.is_empty() {
                    instructions = text;
                } else {
                    instructions.push_str("\n\n");
                    instructions.push_str(&text);
                }
            }
        }
    }

    let vision = model_supports_vision(&req.model);

    let mut input = Vec::new();
    let mut pending_tool_images: Vec<Value> = Vec::new();
    for msg in &req.messages {
        if msg.role != Role::Tool && !pending_tool_images.is_empty() {
            input.push(tool_images_item(std::mem::take(&mut pending_tool_images)));
        }
        match msg.role {
            Role::User => {
                let has_images = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. }));
                if !vision || !has_images {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": crate::image::text_with_image_fallbacks(msg),
                        }]
                    }));
                    continue;
                }
                let mut text = msg.text();
                let mut content: Vec<Value> = Vec::new();
                for block in &msg.content {
                    if let ContentBlock::Image { source } = block {
                        match crate::openai_compat::load_image_payload(source) {
                            Some((mime, b64)) => content.push(serde_json::json!({
                                "type": "input_image",
                                "image_url": format!("data:{mime};base64,{b64}"),
                            })),
                            None => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(&crate::image::image_fallback_text(source));
                            }
                        }
                    }
                }
                let mut parts = Vec::with_capacity(content.len() + 1);
                parts.push(serde_json::json!({
                    "type": "input_text",
                    "text": text,
                }));
                parts.extend(content);
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": parts,
                }));
            }
            Role::Assistant => {
                // `input_image` is only valid on user input; degrade to text.
                let text = crate::image::text_with_image_fallbacks(msg);
                if !text.is_empty() {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": text,
                        }]
                    }));
                }
                for block in &msg.content {
                    if let ContentBlock::ToolUse {
                        id,
                        name,
                        input: tool_input,
                    } = block
                    {
                        let args = if tool_input.is_object() || tool_input.is_array() {
                            tool_input.to_string()
                        } else {
                            "{}".to_string()
                        };
                        input.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": id.as_str(),
                            "name": name,
                            "arguments": args,
                        }));
                    }
                }
            }
            Role::Tool => {
                for block in &msg.content {
                    match block {
                        ContentBlock::ToolResult {
                            call_id,
                            content,
                            is_error,
                        } => {
                            let output = if *is_error {
                                format!("[Error] {content}")
                            } else {
                                content.clone()
                            };
                            input.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": call_id.as_str(),
                                "output": output,
                            }));
                        }
                        ContentBlock::Image { source } => pending_tool_images.push(if vision {
                            image_part(source)
                        } else {
                            serde_json::json!({
                                "type": "input_text",
                                "text": crate::image::image_fallback_text(source),
                            })
                        }),
                        ContentBlock::Text { .. }
                        | ContentBlock::Reasoning { .. }
                        | ContentBlock::ToolUse { .. } => {}
                    }
                }
            }
            Role::System => {}
        }
    }
    if !pending_tool_images.is_empty() {
        input.push(tool_images_item(pending_tool_images));
    }

    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), serde_json::json!(req.model));
    body.insert("instructions".to_string(), serde_json::json!(instructions));
    body.insert("input".to_string(), Value::Array(input));
    body.insert("stream".to_string(), serde_json::json!(true));
    body.insert("store".to_string(), serde_json::json!(false));

    if !req.tools.is_empty() {
        let tools_val: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body.insert("tools".to_string(), Value::Array(tools_val));
        body.insert("tool_choice".to_string(), serde_json::json!("auto"));
    }

    if !is_oauth && let Some(max_tokens) = req.max_output_tokens {
        body.insert(
            "max_output_tokens".to_string(),
            serde_json::json!(max_tokens),
        );
    }

    if reasoning_enabled_for(cfg, &req.model) {
        let effort = req.effort.unwrap_or(defaults.effort);
        let effort_str = match cfg.effort_map.get(effort.as_str()) {
            Some(override_val) => override_val.clone(),
            None => match effort {
                Effort::Low => "low".to_string(),
                Effort::Medium => "medium".to_string(),
                Effort::High | Effort::XHigh | Effort::Max => "high".to_string(),
            },
        };
        body.insert(
            "reasoning".to_string(),
            serde_json::json!({
                "effort": effort_str,
                "summary": "auto"
            }),
        );
    }

    let mut body_val = Value::Object(body);
    if let Some(extra) = &cfg.extra_body
        && extra.is_object()
    {
        deep_merge(&mut body_val, extra);
    }
    body_val
}

pub fn reasoning_enabled_for(cfg: &ProviderConfig, model: &str) -> bool {
    let model_cfg = cfg.models.iter().find(|m| m.id == model);
    if let Some(m) = model_cfg
        && let Some(r) = m.reasoning
    {
        return r;
    }
    if let Some(r) = cfg.reasoning {
        return r;
    }
    model_heuristic_reasoning(model)
}

pub fn model_heuristic_reasoning(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    REASONING_KEYWORDS.iter().any(|&k| lower.contains(k))
}

/// Whether the model accepts `input_image` parts. The Responses API serves the
/// gpt-5/codex/o-series families, all of which take image input; `o1-mini` and
/// `o1-preview` are the exceptions.
pub fn model_supports_vision(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    if lower.contains("o1-mini") || lower.contains("o1-preview") {
        return false;
    }
    ["codex", "gpt-5", "gpt-4o", "gpt-4.1", "o1", "o3", "o4"]
        .iter()
        .any(|&k| lower.contains(k))
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

/// `input_image` part, or an `input_text` note when the bytes are gone.
fn image_part(source: &ImageSource) -> Value {
    match crate::openai_compat::load_image_payload(source) {
        Some((mime, b64)) => serde_json::json!({
            "type": "input_image",
            "image_url": format!("data:{mime};base64,{b64}"),
        }),
        None => serde_json::json!({
            "type": "input_text",
            "text": crate::image::image_fallback_text(source),
        }),
    }
}

fn tool_images_item(parts: Vec<Value>) -> Value {
    let mut content = vec![serde_json::json!({
        "type": "input_text",
        "text": "Images returned by the tool call(s) above:",
    })];
    content.extend(parts);
    serde_json::json!({ "type": "message", "role": "user", "content": content })
}
