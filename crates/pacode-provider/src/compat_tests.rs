use std::collections::BTreeMap;

use pacode_types::{
    CallId, ContentBlock, Effort, Message, ProviderConfig, ProviderDefaults, Role, ToolDefinition,
};
use serde_json::json;

use super::*;
use crate::{CompletionRequest, OpenAiCompat};

fn provider(base_url: &str) -> OpenAiCompat {
    let cfg = ProviderConfig {
        base_url: base_url.to_string(),
        ..ProviderConfig::default()
    };
    OpenAiCompat::new("p", cfg, ProviderDefaults::default(), None, BTreeMap::new())
        .expect("provider builds")
}

fn request(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: model.to_string(),
        system_static: "sys".to_string(),
        system_dynamic: String::new(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        effort: Some(Effort::Medium),
        max_output_tokens: Some(1000),
    }
}

fn tool_turn(id: &str) -> Vec<Message> {
    vec![
        Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: CallId::new(id),
                name: "ls".to_string(),
                input: json!({}),
            }],
        ),
        Message::tool_result(CallId::new(id), "ok", false),
    ]
}

#[test]
fn host_of_strips_scheme_port_path_and_credentials() {
    assert_eq!(host_of("https://API.Z.ai/api/paas/v4"), "api.z.ai");
    assert_eq!(host_of("http://localhost:11434/v1"), "localhost");
    assert_eq!(
        host_of("https://user:pw@example.test:8443/x?y#z"),
        "example.test"
    );
    assert_eq!(host_of("example.test/v1"), "example.test");
}

#[test]
fn for_base_url_matches_builtin_hosts_and_defaults_otherwise() {
    let kimi = Compat::for_base_url("https://api.kimi.com/coding/v1");
    assert_eq!(kimi.default_max_tokens, Some(32_768));
    let moonshot_cn = Compat::for_base_url("https://api.moonshot.cn/v1");
    assert_eq!(moonshot_cn.default_max_tokens, Some(32_768));
    let openai = Compat::for_base_url("https://api.openai.com/v1");
    assert_eq!(openai.max_tokens_field, MaxTokensField::MaxCompletionTokens);
    let local = Compat::for_base_url("http://localhost:1234/v1");
    assert_eq!(local.max_tokens_field, MaxTokensField::MaxTokens);
    assert_eq!(local.default_max_tokens, None);
}

#[test]
fn tool_id_sanitizes_charset_and_keeps_tail_within_limit() {
    let openai = Compat::for_base_url("https://api.openai.com/v1");
    assert_eq!(openai.tool_id("call_1"), "call_1");
    assert_eq!(openai.tool_id("call|fc.1"), "call_fc_1");
    let long = format!("{}-tail", "x".repeat(60));
    let out = openai.tool_id(&long);
    assert_eq!(out.chars().count(), 40);
    assert!(out.ends_with("-tail"));
    assert_eq!(Compat::DEFAULT.tool_id("a|b"), "a|b");
}

#[test]
fn gemini_strips_schema_keywords_and_model_prefix() {
    let gemini = Compat::for_base_url("https://generativelanguage.googleapis.com/v1beta/openai");
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "additionalProperties": false,
        "properties": { "a": { "type": "object", "additionalProperties": false } }
    });
    let cleaned = gemini.clean_schema(&schema);
    assert!(cleaned.get("$schema").is_none());
    assert!(cleaned.get("additionalProperties").is_none());
    assert!(
        cleaned["properties"]["a"]
            .get("additionalProperties")
            .is_none()
    );
    assert_eq!(cleaned["type"], "object");
    assert_eq!(gemini.model_id("models/gemini-2.5-pro"), "gemini-2.5-pro");
    assert_eq!(gemini.model_id("gemini-2.5-pro"), "gemini-2.5-pro");
    assert!(gemini.heuristic_reasoning("gemini-2.5-flash"));
    assert!(!gemini.heuristic_reasoning("gemini-2.0-flash"));
}

#[test]
fn openai_uses_max_completion_tokens() {
    let body = provider("https://api.openai.com/v1").build_body(&request("gpt-5.5"));
    assert_eq!(body["max_completion_tokens"], 1000);
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["reasoning_effort"], "medium");
}

#[test]
fn openrouter_nests_reasoning_effort() {
    let body = provider("https://openrouter.ai/api/v1").build_body(&request("openai/gpt-5.5"));
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["max_tokens"], 1000);
}

#[test]
fn zai_toggles_thinking() {
    let p = provider("https://api.z.ai/api/coding/paas/v4");
    let body = p.build_body(&request("glm-5.3"));
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(body.get("reasoning_effort").is_none());
    let plain = p.build_body(&request("glm-4-plus"));
    assert!(plain.get("thinking").is_none());
}

#[test]
fn kimi_k3_uses_tiered_effort_and_k2_toggles() {
    let p = provider("https://api.moonshot.ai/v1");
    let k3 = p.build_body(&request("kimi-k3"));
    assert_eq!(k3["reasoning_effort"], "high");
    let mut max_req = request("kimi-k3");
    max_req.effort = Some(Effort::Max);
    assert_eq!(p.build_body(&max_req)["reasoning_effort"], "max");
    let k2 = p.build_body(&request("kimi-k2.7-code"));
    assert_eq!(k2["thinking"]["type"], "enabled");
}

#[test]
fn kimi_always_sends_max_tokens_and_pads_tool_turns() {
    let p = provider("https://api.kimi.com/coding/v1");
    let mut req = request("kimi-for-coding");
    req.max_output_tokens = None;
    req.messages.extend(tool_turn("call_1"));
    let body = p.build_body(&req);
    assert_eq!(body["max_tokens"], 32_768);
    let assistant = &body["messages"][2];
    assert_eq!(assistant["content"], ".");
    assert_eq!(assistant["reasoning_content"], ".");
}

#[test]
fn default_endpoint_keeps_plain_openai_shape() {
    let p = provider("http://localhost:11434/v1");
    let mut req = request("llama3");
    req.messages.extend(tool_turn("call|1"));
    let body = p.build_body(&req);
    assert_eq!(body["max_tokens"], 1000);
    let assistant = &body["messages"][2];
    assert!(assistant["content"].is_null());
    assert!(assistant.get("reasoning_content").is_none());
    assert_eq!(assistant["tool_calls"][0]["id"], "call|1");
}

#[test]
fn tool_ids_stay_paired_after_sanitizing() {
    let p = provider("https://openrouter.ai/api/v1");
    let mut req = request("openai/gpt-5.5");
    req.messages.extend(tool_turn("call|odd.id"));
    req.tools = vec![ToolDefinition {
        name: "ls".to_string(),
        description: "d".to_string(),
        input_schema: json!({"type": "object"}),
    }];
    let body = p.build_body(&req);
    let msgs = body["messages"].as_array().expect("messages");
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_odd_id");
    assert_eq!(msgs[3]["tool_call_id"], "call_odd_id");
}

#[test]
fn effort_str_tables() {
    assert_eq!(
        Compat::effort_str(ReasoningWire::ReasoningEffort, Effort::Max),
        "high"
    );
    assert_eq!(
        Compat::effort_str(ReasoningWire::ReasoningEffortTiered, Effort::Medium),
        "high"
    );
    assert_eq!(
        Compat::effort_str(ReasoningWire::ReasoningEffortTiered, Effort::Low),
        "low"
    );
}
