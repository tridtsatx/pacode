use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{
    CallId, ContentBlock, Effort, Message, ModelConfig, ProviderConfig, ProviderDefaults, Role,
    StopReason, StreamEvent, ToolDefinition,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::stream::{AnthropicState, parse_anthropic_payload};
use super::types::{CLAUDE_CODE_IDENTITY, map_tool_name_for_oauth, map_tool_name_from_oauth};
use super::{Anthropic, AnthropicAuth};
use crate::{CompletionRequest, Provider, ProviderError};

fn make_anthropic_provider(
    base_url: String,
    auth: Option<AnthropicAuth>,
    models: Vec<ModelConfig>,
    reasoning: Option<bool>,
    extra_body: Option<serde_json::Value>,
) -> Anthropic {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url,
        api_key: None,
        api_key_env: None,
        models,
        catalog: true,
        context_window: Some(200_000),
        reasoning,
        effort_map: BTreeMap::new(),
        extra_body,
        headers: BTreeMap::new(),
        proxy: None,
    };
    Anthropic::new(
        "anthropic",
        cfg,
        ProviderDefaults::default(),
        auth,
        BTreeMap::new(),
    )
    .expect("valid anthropic provider")
}

#[test]
fn test_tool_name_remapping_table() {
    let mappings = [
        ("bash", "Bash"),
        ("read", "Read"),
        ("write", "Write"),
        ("edit", "Edit"),
        ("glob", "Glob"),
        ("grep", "Grep"),
        ("subagent", "Agent"),
        ("schedule", "ScheduleWakeup"),
        ("skill_manage", "Skill"),
    ];

    for (native, wire) in mappings {
        assert_eq!(
            map_tool_name_for_oauth(native),
            wire,
            "forward remap for '{native}'"
        );
        assert_eq!(
            map_tool_name_from_oauth(wire),
            native,
            "reverse remap for '{wire}'"
        );
    }

    // Unmapped tools pass through unchanged both ways
    assert_eq!(map_tool_name_for_oauth("custom_tool"), "custom_tool");
    assert_eq!(map_tool_name_from_oauth("custom_tool"), "custom_tool");
}

#[test]
fn test_request_shape_api_key_mode() {
    let auth = AnthropicAuth::ApiKey("sk-ant-api-testkey123".to_string());
    let provider = make_anthropic_provider(
        "https://api.anthropic.com/v1".to_string(),
        Some(auth),
        vec![],
        None,
        None,
    );

    assert!(!provider.is_oauth());
    assert_eq!(
        provider.build_url(),
        "https://api.anthropic.com/v1/messages"
    );

    let headers = provider.build_headers();
    let header_map: BTreeMap<String, String> = headers.into_iter().collect();

    assert_eq!(header_map.get("anthropic-version").unwrap(), "2023-06-01");
    assert_eq!(
        header_map.get("x-api-key").unwrap(),
        "sk-ant-api-testkey123"
    );
    assert!(!header_map.contains_key("Authorization"));
    assert!(!header_map.contains_key("anthropic-beta"));
    assert!(header_map.get("User-Agent").unwrap().starts_with("pacode/"));

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        system_static: "Static prompt".to_string(),
        system_dynamic: "Dynamic prompt".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![ToolDefinition {
            name: "bash".to_string(),
            description: "Run command".to_string(),
            input_schema: json!({"type": "object"}),
        }],
        effort: None,
        max_output_tokens: Some(4096),
    };

    let body = provider.build_body(&req);

    // System prompt: static first, dynamic second; NO Claude Code identity
    let system = body["system"].as_array().expect("system array");
    assert_eq!(system.len(), 2);
    assert_eq!(system[0]["text"], "Static prompt");
    assert_eq!(system[1]["text"], "Dynamic prompt");

    // Tools: NOT remapped
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools[0]["name"], "bash");

    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["stream"], true);
}

#[test]
fn test_request_shape_oauth_mode() {
    let auth = AnthropicAuth::OAuth("sk-ant-acc-testoauth456".to_string());
    let provider = make_anthropic_provider(
        "https://api.anthropic.com/v1".to_string(),
        Some(auth),
        vec![],
        None,
        None,
    );

    assert!(provider.is_oauth());
    assert_eq!(
        provider.build_url(),
        "https://api.anthropic.com/v1/messages?beta=true"
    );

    let headers = provider.build_headers();
    let header_map: BTreeMap<String, String> = headers.into_iter().collect();

    assert_eq!(header_map.get("anthropic-version").unwrap(), "2023-06-01");
    assert_eq!(
        header_map.get("Authorization").unwrap(),
        "Bearer sk-ant-acc-testoauth456"
    );
    assert!(!header_map.contains_key("x-api-key"));
    assert_eq!(header_map.get("User-Agent").unwrap(), "claude-cli/1.0.0");
    assert_eq!(
        header_map.get("anthropic-beta").unwrap(),
        "oauth-2025-04-20,claude-code-20250219"
    );

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        system_static: "Static prompt".to_string(),
        system_dynamic: "Dynamic prompt".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![
            ToolDefinition {
                name: "bash".to_string(),
                description: "Run command".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "read".to_string(),
                description: "Read file".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "subagent".to_string(),
                description: "Subagent".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "custom_tool".to_string(),
                description: "Custom".to_string(),
                input_schema: json!({"type": "object"}),
            },
        ],
        effort: None,
        max_output_tokens: None,
    };

    let body = provider.build_body(&req);

    // System prompt: FIRST block is Claude Code identity line, then static, then dynamic
    let system = body["system"].as_array().expect("system array");
    assert_eq!(system.len(), 3);
    assert_eq!(system[0]["text"], CLAUDE_CODE_IDENTITY);
    assert_eq!(system[1]["text"], "Static prompt");
    assert_eq!(system[2]["text"], "Dynamic prompt");

    // Tools: remapped according to the table
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools[0]["name"], "Bash");
    assert_eq!(tools[1]["name"], "Read");
    assert_eq!(tools[2]["name"], "Agent");
    assert_eq!(tools[3]["name"], "custom_tool");
}

#[test]
fn test_messages_conversion_and_role_merging() {
    let auth = AnthropicAuth::OAuth("sk-ant-acc-test".to_string());
    let provider = make_anthropic_provider(
        "https://api.anthropic.com/v1".to_string(),
        Some(auth),
        vec![],
        None,
        None,
    );

    let call_id = CallId::new("call_123");
    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        system_static: String::new(),
        system_dynamic: String::new(),
        messages: vec![
            Message::user("Please list files"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "I should run ls".to_string(),
                        signature: Some("sig_abc".to_string()),
                    },
                    ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: "bash".to_string(),
                        input: json!({"command": "ls"}),
                    },
                ],
                meta: pacode_types::MessageMeta::default(),
            },
            Message::tool_result(call_id, "file1.rs\nfile2.rs", false),
            Message::user("Now what?"),
        ],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let body = provider.build_body(&req);
    let msgs = body["messages"].as_array().expect("messages array");

    // msg 0: user "Please list files"
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"][0]["text"], "Please list files");

    // msg 1: assistant with thinking and tool_use (remapped to "Bash")
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][0]["type"], "thinking");
    assert_eq!(msgs[1]["content"][0]["thinking"], "I should run ls");
    assert_eq!(msgs[1]["content"][0]["signature"], "sig_abc");
    assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][1]["name"], "Bash");
    assert_eq!(msgs[1]["content"][1]["id"], "call_123");

    // msg 2: tool result merged with subsequent user message into ONE user turn
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "call_123");
    assert_eq!(msgs[2]["content"][0]["content"], "file1.rs\nfile2.rs");
    assert_eq!(msgs[2]["content"][1]["type"], "text");
    assert_eq!(msgs[2]["content"][1]["text"], "Now what?");

    // Total alternating messages: 3
    assert_eq!(msgs.len(), 3);
}

#[test]
fn test_trailing_assistant_turn_repaired_with_continue() {
    let auth = AnthropicAuth::ApiKey("key".to_string());
    let provider = make_anthropic_provider(
        "https://api.anthropic.com/v1".to_string(),
        Some(auth),
        vec![],
        None,
        None,
    );

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        system_static: String::new(),
        system_dynamic: String::new(),
        messages: vec![
            Message::user("Hello"),
            Message::assistant_text("Thinking..."),
        ],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let body = provider.build_body(&req);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["text"], "Continue.");
}

#[test]
fn test_reasoning_effort_budget_mapping() {
    let provider = make_anthropic_provider(
        "https://api.anthropic.com/v1".to_string(),
        Some(AnthropicAuth::ApiKey("key".to_string())),
        vec![ModelConfig {
            id: "claude-3-7-sonnet".to_string(),
            display_name: None,
            context_window: None,
            reasoning: Some(true),
        }],
        None,
        None,
    );

    let check_budget = |effort: Effort, expected_budget: u64| {
        let req = CompletionRequest {
            model: "claude-3-7-sonnet".to_string(),
            system_static: "".to_string(),
            system_dynamic: "".to_string(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            effort: Some(effort),
            max_output_tokens: Some(32000),
        };
        let body = provider.build_body(&req);
        assert_eq!(
            body["thinking"]["budget_tokens"].as_u64().unwrap(),
            expected_budget,
            "effort {effort:?} budget"
        );
        assert_eq!(body["thinking"]["type"], "enabled");
    };

    check_budget(Effort::Low, 1024);
    check_budget(Effort::Medium, 4096);
    check_budget(Effort::High, 8192);
    check_budget(Effort::Max, 16384);
}

#[test]
fn test_sse_decode_full_stream_oauth_tool_reverse_remap() {
    let mut state = AnthropicState::default();

    // 1. message_start
    let start_json = json!({
        "type": "message_start",
        "message": {
            "model": "claude-3-7-sonnet-20250219",
            "usage": {
                "input_tokens": 120,
                "cache_read_input_tokens": 40,
                "cache_creation_input_tokens": 10
            }
        }
    });
    let ev1 = parse_anthropic_payload(&start_json, &mut state, true).expect("parsed");
    assert!(ev1.is_empty());
    assert_eq!(state.model.as_deref(), Some("claude-3-7-sonnet-20250219"));
    assert_eq!(state.input_tokens, 120);
    assert_eq!(state.cache_read_tokens, 40);
    assert_eq!(state.cache_write_tokens, 10);

    // 2. content_block_start thinking
    let thinking_start_json = json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "thinking",
            "thinking": ""
        }
    });
    let ev2 = parse_anthropic_payload(&thinking_start_json, &mut state, true).expect("parsed");
    assert!(ev2.is_empty());

    // 3. content_block_delta thinking
    let thinking_delta_json = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {
            "type": "thinking_delta",
            "thinking": "Analyzing file structure..."
        }
    });
    let ev3 = parse_anthropic_payload(&thinking_delta_json, &mut state, true).expect("parsed");
    assert_eq!(
        ev3,
        vec![StreamEvent::ReasoningDelta {
            text: "Analyzing file structure...".to_string()
        }]
    );

    // 4. content_block_stop thinking
    let stop0 = json!({ "type": "content_block_stop", "index": 0 });
    let _ = parse_anthropic_payload(&stop0, &mut state, true).expect("parsed");

    // 5. content_block_start text
    let text_start_json = json!({
        "type": "content_block_start",
        "index": 1,
        "content_block": {
            "type": "text",
            "text": ""
        }
    });
    let _ = parse_anthropic_payload(&text_start_json, &mut state, true).expect("parsed");

    // 6. content_block_delta text
    let text_delta_json = json!({
        "type": "content_block_delta",
        "index": 1,
        "delta": {
            "type": "text_delta",
            "text": "Running the list tool:"
        }
    });
    let ev6 = parse_anthropic_payload(&text_delta_json, &mut state, true).expect("parsed");
    assert_eq!(
        ev6,
        vec![StreamEvent::TextDelta {
            text: "Running the list tool:".to_string()
        }]
    );

    // 7. content_block_start tool_use (wire name "Bash" in OAuth mode)
    let tool_start_json = json!({
        "type": "content_block_start",
        "index": 2,
        "content_block": {
            "type": "tool_use",
            "id": "toolu_abc123",
            "name": "Bash"
        }
    });
    let ev7 = parse_anthropic_payload(&tool_start_json, &mut state, true).expect("parsed");
    // Wire name "Bash" must be reverse-remapped back to "bash"!
    assert_eq!(
        ev7,
        vec![StreamEvent::ToolCallStart {
            index: 2,
            id: CallId::new("toolu_abc123"),
            name: "bash".to_string(),
        }]
    );

    // 8. content_block_delta input_json
    let input_delta_json = json!({
        "type": "content_block_delta",
        "index": 2,
        "delta": {
            "type": "input_json_delta",
            "partial_json": "{\"command\":\"ls\"}"
        }
    });
    let ev8 = parse_anthropic_payload(&input_delta_json, &mut state, true).expect("parsed");
    assert_eq!(
        ev8,
        vec![StreamEvent::ToolCallArgsDelta {
            index: 2,
            delta: "{\"command\":\"ls\"}".to_string(),
        }]
    );

    // 9. message_delta with usage and stop_reason
    let msg_delta_json = json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": "tool_use"
        },
        "usage": {
            "output_tokens": 45
        }
    });
    let ev9 = parse_anthropic_payload(&msg_delta_json, &mut state, true).expect("parsed");
    assert_eq!(ev9.len(), 2);
    match &ev9[0] {
        StreamEvent::Usage(u) => {
            assert_eq!(u.input_tokens, 120);
            assert_eq!(u.output_tokens, 45);
            assert_eq!(u.cache_read_tokens, 40);
            assert_eq!(u.cache_write_tokens, 10);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
    assert_eq!(
        ev9[1],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );
    assert!(state.message_end_emitted);

    // 10. message_stop
    let stop_json = json!({ "type": "message_stop" });
    let ev10 = parse_anthropic_payload(&stop_json, &mut state, true).expect("parsed");
    assert!(ev10.is_empty());
    assert!(state.stream_ended);
}

#[test]
fn test_error_mapping_in_stream() {
    let mut state = AnthropicState::default();

    // 1. authentication_error -> ProviderError::Auth
    let auth_err = json!({
        "type": "error",
        "error": {
            "type": "authentication_error",
            "message": "Invalid x-api-key"
        }
    });
    match parse_anthropic_payload(&auth_err, &mut state, false) {
        Err(ProviderError::Auth(msg)) => assert!(msg.contains("Invalid x-api-key")),
        other => panic!("expected ProviderError::Auth, got {other:?}"),
    }

    // 2. rate_limit_error -> ProviderError::RateLimited
    let rate_err = json!({
        "type": "error",
        "error": {
            "type": "rate_limit_error",
            "message": "Rate limit exceeded"
        }
    });
    match parse_anthropic_payload(&rate_err, &mut state, false) {
        Err(ProviderError::RateLimited(msg)) => assert!(msg.contains("Rate limit exceeded")),
        other => panic!("expected ProviderError::RateLimited, got {other:?}"),
    }

    // 3. overloaded_error -> ProviderError::RateLimited (retryable)
    let over_err = json!({
        "type": "error",
        "error": {
            "type": "overloaded_error",
            "message": "Anthropic is currently overloaded"
        }
    });
    match parse_anthropic_payload(&over_err, &mut state, false) {
        Err(err) => {
            assert!(err.is_retryable());
            match err {
                ProviderError::RateLimited(msg) => assert!(msg.contains("overloaded")),
                other => panic!("expected ProviderError::RateLimited, got {other:?}"),
            }
        }
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn test_complete_stream_mock_server_api_key() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]);

        // Assert request line and headers
        assert!(req_str.starts_with("POST /messages HTTP/1.1"));
        assert!(!req_str.contains("?beta=true"));
        assert!(req_str.contains("x-api-key: my-secret-key"));
        assert!(req_str.contains("anthropic-version: 2023-06-01"));

        // Stream back Anthropic SSE
        let events = [
            "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-3-5-sonnet\",\"usage\":{\"input_tokens\":10}}}\r\n\r\n",
            "event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n\r\n",
            "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\r\n\r\n",
            "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\r\n\r\n",
            "event: content_block_stop\r\ndata: {\"type\":\"content_block_stop\",\"index\":0}\r\n\r\n",
            "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\r\n\r\n",
            "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
        ];

        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        for ev in events {
            socket.write_all(ev.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    let provider = make_anthropic_provider(
        format!("http://127.0.0.1:{port}"),
        Some(AnthropicAuth::ApiKey("my-secret-key".to_string())),
        vec![],
        None,
        None,
    );

    let req = CompletionRequest {
        model: "claude-3-5-sonnet".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete ok");
    let mut received = Vec::new();
    while let Some(event) = stream.next().await {
        received.push(event.expect("event ok"));
    }
    server.await.unwrap();

    // Verify events
    assert!(matches!(
        received[0],
        StreamEvent::MessageStart {
            model: Some(ref m)
        } if m == "claude-3-5-sonnet"
    ));
    assert_eq!(
        received[1],
        StreamEvent::TextDelta {
            text: "Hello ".to_string()
        }
    );
    assert_eq!(
        received[2],
        StreamEvent::TextDelta {
            text: "world!".to_string()
        }
    );
    match &received[3] {
        StreamEvent::Usage(u) => {
            assert_eq!(u.input_tokens, 10);
            assert_eq!(u.output_tokens, 5);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
    assert_eq!(
        received[4],
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );
}

#[tokio::test]
async fn test_complete_stream_mock_server_oauth() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]);

        // Assert OAuth contract
        assert!(req_str.starts_with("POST /messages?beta=true HTTP/1.1"));
        assert!(req_str.contains("authorization: Bearer my-oauth-token"));
        assert!(req_str.contains("user-agent: claude-cli/1.0.0"));
        assert!(req_str.contains("anthropic-beta: oauth-2025-04-20,claude-code-20250219"));

        // Body should have Claude Code identity line as first system block
        assert!(req_str.contains(CLAUDE_CODE_IDENTITY));

        // Tool remapping in request body: "bash" was remapped to "Bash"
        assert!(req_str.contains("\"name\":\"Bash\""));

        // Server responds with SSE stream yielding "Agent" tool use (subagent)
        let events = [
            "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4\",\"usage\":{\"input_tokens\":50}}}\r\n\r\n",
            "event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_sub_1\",\"name\":\"Agent\"}}\r\n\r\n",
            "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"prompt\\\":\\\"do work\\\"}\"}}\r\n\r\n",
            "event: content_block_stop\r\ndata: {\"type\":\"content_block_stop\",\"index\":0}\r\n\r\n",
            "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\r\n\r\n",
            "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
        ];

        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        for ev in events {
            socket.write_all(ev.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    let provider = make_anthropic_provider(
        format!("http://127.0.0.1:{port}"),
        Some(AnthropicAuth::OAuth("my-oauth-token".to_string())),
        vec![],
        None,
        None,
    );

    let req = CompletionRequest {
        model: "claude-opus-4".to_string(),
        system_static: "Static sys".to_string(),
        system_dynamic: String::new(),
        messages: vec![Message::user("Run agent")],
        tools: vec![ToolDefinition {
            name: "bash".to_string(),
            description: "Bash tool".to_string(),
            input_schema: json!({"type": "object"}),
        }],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete ok");
    let mut received = Vec::new();
    while let Some(event) = stream.next().await {
        received.push(event.expect("event ok"));
    }
    server.await.unwrap();

    // Verify response: "Agent" was reverse-mapped to "subagent"!
    assert!(matches!(received[0], StreamEvent::MessageStart { .. }));
    assert_eq!(
        received[1],
        StreamEvent::ToolCallStart {
            index: 0,
            id: CallId::new("call_sub_1"),
            name: "subagent".to_string(),
        }
    );
    assert_eq!(
        received[2],
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "{\"prompt\":\"do work\"}".to_string(),
        }
    );
    assert_eq!(
        received[4],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );
}

#[tokio::test]
async fn test_http_status_errors_and_overloaded() {
    // 1. 401 Unauthorized
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 21\r\nConnection: close\r\n\r\ninvalid api key text";
            socket.write_all(resp.as_bytes()).await.unwrap();
        });

        let provider = make_anthropic_provider(
            format!("http://127.0.0.1:{port}"),
            Some(AnthropicAuth::ApiKey("bad-key".to_string())),
            vec![],
            None,
            None,
        );

        let req = CompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            system_static: "".to_string(),
            system_dynamic: "".to_string(),
            messages: vec![],
            tools: vec![],
            effort: None,
            max_output_tokens: None,
        };

        match provider.complete(req).await {
            Err(ProviderError::Auth(msg)) => assert!(msg.contains("401")),
            Err(other) => panic!("expected ProviderError::Auth, got {other:?}"),
            Ok(_) => panic!("expected ProviderError::Auth, got Ok"),
        }
    }

    // 2. 429 Rate Limited (with max_retries = 0 so it fails immediately)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let resp = "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 12\r\nConnection: close\r\n\r\nrate limited";
            socket.write_all(resp.as_bytes()).await.unwrap();
        });

        let cfg = ProviderConfig {
            kind: Default::default(),
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            api_key_env: None,
            models: vec![],
            catalog: false,
            context_window: None,
            reasoning: None,
            effort_map: BTreeMap::new(),
            extra_body: None,
            headers: BTreeMap::new(),
            proxy: None,
        };
        let defaults = ProviderDefaults {
            max_retries: 0,
            ..ProviderDefaults::default()
        };
        let provider = Anthropic::new(
            "anthropic",
            cfg,
            defaults,
            Some(AnthropicAuth::ApiKey("key".to_string())),
            BTreeMap::new(),
        )
        .unwrap();

        let req = CompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            system_static: "".to_string(),
            system_dynamic: "".to_string(),
            messages: vec![],
            tools: vec![],
            effort: None,
            max_output_tokens: None,
        };

        match provider.complete(req).await {
            Err(ProviderError::RateLimited(msg)) => assert!(msg.contains("rate limited")),
            Err(other) => panic!("expected ProviderError::RateLimited, got {other:?}"),
            Ok(_) => panic!("expected ProviderError::RateLimited, got Ok"),
        }
    }

    // 3. 529 Overloaded (Anthropic's HTTP status for overloaded; max_retries = 0)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let resp = "HTTP/1.1 529 Overloaded\r\nContent-Length: 10\r\nConnection: close\r\n\r\noverloaded";
            socket.write_all(resp.as_bytes()).await.unwrap();
        });

        let cfg = ProviderConfig {
            kind: Default::default(),
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            api_key_env: None,
            models: vec![],
            catalog: false,
            context_window: None,
            reasoning: None,
            effort_map: BTreeMap::new(),
            extra_body: None,
            headers: BTreeMap::new(),
            proxy: None,
        };
        let defaults = ProviderDefaults {
            max_retries: 0,
            ..ProviderDefaults::default()
        };
        let provider = Anthropic::new(
            "anthropic",
            cfg,
            defaults,
            Some(AnthropicAuth::ApiKey("key".to_string())),
            BTreeMap::new(),
        )
        .unwrap();

        let req = CompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            system_static: "".to_string(),
            system_dynamic: "".to_string(),
            messages: vec![],
            tools: vec![],
            effort: None,
            max_output_tokens: None,
        };

        match provider.complete(req).await {
            Err(err) => {
                assert!(err.is_retryable());
                match err {
                    ProviderError::RateLimited(msg) => assert!(msg.contains("overloaded")),
                    ProviderError::Http { status: 529, .. } => {}
                    other => panic!("expected RateLimited or Http 529, got {other:?}"),
                }
            }
            Ok(_) => panic!("expected error"),
        }
    }
}

#[tokio::test]
async fn test_retry_recovers_from_500_server_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Attempt 0: return 500 Internal Server Error
        let (mut socket0, _) = listener.accept().await.unwrap();
        let mut buf0 = [0u8; 1024];
        let _ = socket0.read(&mut buf0).await.unwrap();
        let resp0 = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 14\r\nConnection: close\r\n\r\ninternal error";
        socket0.write_all(resp0.as_bytes()).await.unwrap();
        drop(socket0);

        // Attempt 1: return 200 OK with SSE
        let (mut socket1, _) = listener.accept().await.unwrap();
        let mut buf1 = [0u8; 1024];
        let _ = socket1.read(&mut buf1).await.unwrap();
        let sse = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-3-5-sonnet\",\"usage\":{\"input_tokens\":5}}}\r\n\r\n\
                   event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\r\n\r\n\
                   event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\r\n\r\n\
                   event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
        let resp1 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            sse.len(),
            sse
        );
        socket1.write_all(resp1.as_bytes()).await.unwrap();
    });

    let provider = make_anthropic_provider(
        format!("http://127.0.0.1:{port}"),
        Some(AnthropicAuth::ApiKey("key".to_string())),
        vec![],
        None,
        None,
    )
    .with_backoff_base(Duration::from_millis(1));

    let req = CompletionRequest {
        model: "claude-3-5-sonnet".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("retried ok");
    let mut saw_text = false;
    while let Some(event) = stream.next().await {
        if let Ok(StreamEvent::TextDelta { text }) = event
            && text == "ok"
        {
            saw_text = true;
        }
    }
    server.await.unwrap();
    assert!(saw_text);
}

#[tokio::test]
async fn test_list_models_catalog_fetch_and_merge() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]);

        assert!(req_str.starts_with("GET /models HTTP/1.1"));
        assert!(req_str.contains("x-api-key: my-key"));

        let body = json!({
            "data": [
                { "id": "claude-3-5-sonnet-20241022", "display_name": "Claude 3.5 Sonnet" },
                { "id": "claude-3-5-haiku-20241022", "display_name": "Claude 3.5 Haiku" }
            ]
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: format!("http://127.0.0.1:{port}"),
        api_key: None,
        api_key_env: None,
        models: vec![ModelConfig {
            id: "claude-3-5-sonnet-20241022".to_string(),
            display_name: Some("Custom Sonnet".to_string()),
            context_window: Some(200_000),
            reasoning: Some(false),
        }],
        catalog: true,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
        proxy: None,
    };

    let provider = Anthropic::new(
        "anthropic",
        cfg,
        ProviderDefaults::default(),
        Some(AnthropicAuth::ApiKey("my-key".to_string())),
        BTreeMap::new(),
    )
    .expect("ok");

    let models = provider.list_models().await.expect("models ok");
    server.await.unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].route.model, "claude-3-5-sonnet-20241022");
    assert_eq!(models[0].display_name, "Custom Sonnet");
    assert_eq!(models[1].route.model, "claude-3-5-haiku-20241022");
    assert_eq!(models[1].context_window, Some(200_000));
}

#[test]
fn test_anthropic_malformed_proxy_names_provider_and_offending_value() {
    let cfg = ProviderConfig {
        base_url: "https://api.anthropic.com/v1".to_string(),
        proxy: Some("bad://invalid:99".to_string()),
        ..Default::default()
    };
    let res = Anthropic::new(
        "claude-prov",
        cfg,
        ProviderDefaults::default(),
        None,
        BTreeMap::new(),
    );
    assert!(res.is_err());
    let err = res.err().unwrap();
    let msg = format!("{err}");
    assert!(
        msg.contains("claude-prov"),
        "error must name provider: {msg}"
    );
    assert!(
        msg.contains("bad://invalid:99"),
        "error must name offending value: {msg}"
    );
}
