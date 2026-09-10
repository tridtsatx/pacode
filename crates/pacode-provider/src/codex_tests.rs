use std::collections::BTreeMap;
use std::time::Duration;

use pacode_types::{
    CallId, ContentBlock, Effort, Message, ModelConfig, ProviderConfig, ProviderDefaults, Role,
    StopReason, StreamEvent, ToolDefinition, Usage,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::codex::auth::{CodexAuth, CodexOAuthTokens, decode_base64_url, extract_account_id};
use crate::codex::events::{
    ResponsesChunkState, extract_responses_error, responses_chunk_to_events,
};
use crate::codex::{CHATGPT_RESPONSES_URL, Codex, OPENAI_RESPONSES_URL};
use crate::{CompletionRequest, Provider, ProviderError};

fn make_test_provider(auth: Option<CodexAuth>, base_url: Option<&str>) -> Codex {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: base_url.unwrap_or("https://api.openai.com/v1").to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![
            ModelConfig {
                id: "gpt-5.1-codex".to_string(),
                display_name: None,
                context_window: Some(128_000),
                reasoning: Some(true),
            },
            ModelConfig {
                id: "custom-plain".to_string(),
                display_name: None,
                context_window: Some(64_000),
                reasoning: Some(false),
            },
        ],
        catalog: true,
        context_window: Some(128_000),
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    Codex::new(
        "codex",
        cfg,
        ProviderDefaults::default(),
        auth,
        BTreeMap::new(),
    )
    .expect("valid provider")
}

// ---------------------------------------------------------------------------
// 1. Endpoint and Header Selection (OAuth vs API-key)
// ---------------------------------------------------------------------------

#[test]
fn test_endpoint_selection_oauth_vs_apikey() {
    // 1. API key mode (default base_url) -> api.openai.com/v1/responses
    let provider_api = make_test_provider(Some(CodexAuth::api_key("sk-test-123")), None);
    assert!(!provider_api.is_oauth());
    assert_eq!(provider_api.endpoint(), OPENAI_RESPONSES_URL);

    // 2. API key mode with custom proxy base_url -> custom/responses
    let provider_custom = make_test_provider(
        Some(CodexAuth::api_key("sk-test-123")),
        Some("http://127.0.0.1:8317/v1"),
    );
    assert!(!provider_custom.is_oauth());
    assert_eq!(
        provider_custom.endpoint(),
        "http://127.0.0.1:8317/v1/responses"
    );

    // 3. OAuth mode -> always fixed chatgpt.com/backend-api/codex/responses
    let oauth_tokens = CodexOAuthTokens {
        access_token: "tok_oauth_abc".to_string(),
        refresh_token: Some("ref_xyz".to_string()),
        id_token: None,
        account_id: Some("acc_12345".to_string()),
    };
    let provider_oauth = make_test_provider(Some(CodexAuth::OAuth(oauth_tokens.clone())), None);
    assert!(provider_oauth.is_oauth());
    assert_eq!(provider_oauth.endpoint(), CHATGPT_RESPONSES_URL);

    // 4. OAuth mode ignores custom base_url
    let provider_oauth_custom = make_test_provider(
        Some(CodexAuth::OAuth(oauth_tokens)),
        Some("http://127.0.0.1:8317/v1"),
    );
    assert!(provider_oauth_custom.is_oauth());
    assert_eq!(provider_oauth_custom.endpoint(), CHATGPT_RESPONSES_URL);
}

#[test]
fn test_headers_selection_oauth_vs_apikey() {
    // API key mode headers
    let provider_api = make_test_provider(Some(CodexAuth::api_key("sk-my-api-key")), None);
    let headers_api = provider_api.build_headers();
    let map_api: BTreeMap<String, String> = headers_api.into_iter().collect();

    assert_eq!(
        map_api.get("Authorization"),
        Some(&"Bearer sk-my-api-key".to_string())
    );
    assert_eq!(
        map_api.get("Content-Type"),
        Some(&"application/json".to_string())
    );
    assert_eq!(
        map_api.get("OpenAI-Beta"),
        Some(&"responses=experimental".to_string())
    );
    assert!(!map_api.contains_key("originator"));
    assert!(!map_api.contains_key("chatgpt-account-id"));

    // OAuth mode headers
    let provider_oauth = make_test_provider(
        Some(CodexAuth::OAuth(CodexOAuthTokens {
            access_token: "oauth-access-token-999".to_string(),
            refresh_token: None,
            id_token: None,
            account_id: Some("chatgpt-account-abc".to_string()),
        })),
        None,
    );
    let headers_oauth = provider_oauth.build_headers();
    let map_oauth: BTreeMap<String, String> = headers_oauth.into_iter().collect();

    assert_eq!(
        map_oauth.get("Authorization"),
        Some(&"Bearer oauth-access-token-999".to_string())
    );
    assert_eq!(
        map_oauth.get("Content-Type"),
        Some(&"application/json".to_string())
    );
    assert_eq!(
        map_oauth.get("OpenAI-Beta"),
        Some(&"responses=experimental".to_string())
    );
    assert_eq!(
        map_oauth.get("originator"),
        Some(&"codex_cli_rs".to_string())
    );
    assert_eq!(
        map_oauth.get("chatgpt-account-id"),
        Some(&"chatgpt-account-abc".to_string())
    );
}

// ---------------------------------------------------------------------------
// 2. JWT Account ID Claim Extraction
// ---------------------------------------------------------------------------

#[test]
fn test_extract_account_id_from_jwt() {
    // Payload: {"https://api.openai.com/auth":{"chatgpt_account_id":"acc_from_jwt_777"},"email":"user@test.com"}
    // Base64Url without padding:
    // eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjX2Zyb21fand0Xzc3NyJ9LCJlbWFpbCI6InVzZXJAdGVzdC5jb20ifQ
    let jwt = "eyJhbGciOiJub25lIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjX2Zyb21fand0Xzc3NyJ9LCJlbWFpbCI6InVzZXJAdGVzdC5jb20ifQ.sig";

    let extracted = extract_account_id(jwt);
    assert_eq!(extracted, Some("acc_from_jwt_777".to_string()));

    // When account_id is not explicitly set in CodexOAuthTokens, it extracts from id_token
    let auth = CodexAuth::OAuth(CodexOAuthTokens {
        access_token: "tok".to_string(),
        refresh_token: None,
        id_token: Some(jwt.to_string()),
        account_id: None,
    });
    assert_eq!(auth.account_id(), Some("acc_from_jwt_777".to_string()));

    // Invalid tokens
    assert_eq!(extract_account_id("not-a-jwt"), None);
    assert_eq!(extract_account_id("part1.part2"), None);
    assert_eq!(
        extract_account_id("eyJhbGciOiJub25lIn0.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig"),
        None
    );
}

#[test]
fn test_base64_url_decode_padding_variants() {
    // Test base64url decoder without padding and with padding
    assert_eq!(decode_base64_url(""), Some(vec![]));
    assert_eq!(decode_base64_url("YQ"), Some(b"a".to_vec()));
    assert_eq!(decode_base64_url("YWE"), Some(b"aa".to_vec()));
    assert_eq!(decode_base64_url("YWFh"), Some(b"aaa".to_vec()));
}

// ---------------------------------------------------------------------------
// 3. Request Body: Instructions, Input Mapping, Tools, Reasoning
// ---------------------------------------------------------------------------

#[test]
fn test_build_body_instructions_and_input_mapping() {
    let provider = make_test_provider(Some(CodexAuth::api_key("test-key")), None);

    let call_id = CallId::new("call_read_1");
    let req = CompletionRequest {
        model: "custom-plain".to_string(),
        system_static: "Base instructions".to_string(),
        system_dynamic: "Current workspace: /project".to_string(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Check src/main.rs".to_string(),
                }],
                meta: Default::default(),
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Looking into it...".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: "read_file".to_string(),
                        input: json!({"path": "src/main.rs"}),
                    },
                ],
                meta: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: call_id.clone(),
                    content: "fn main() {}".to_string(),
                    is_error: false,
                }],
                meta: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: CallId::new("call_failed_2"),
                    content: "Permission denied".to_string(),
                    is_error: true,
                }],
                meta: Default::default(),
            },
        ],
        tools: vec![],
        effort: None,
        max_output_tokens: Some(2048),
    };

    let body = provider.build_body(&req);

    // 1. Top-level flags
    assert_eq!(body["model"], "custom-plain");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["max_output_tokens"], 2048);

    // 2. Instructions = static + "\n\n" + dynamic
    assert_eq!(
        body["instructions"],
        "Base instructions\n\nCurrent workspace: /project"
    );

    // 3. Input items mapping
    let input = body["input"].as_array().expect("input array");
    assert_eq!(input.len(), 5);

    // User message
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(input[0]["content"][0]["text"], "Check src/main.rs");

    // Assistant message text
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[1]["role"], "assistant");
    assert_eq!(input[1]["content"][0]["type"], "output_text");
    assert_eq!(input[1]["content"][0]["text"], "Looking into it...");

    // Assistant function_call
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_read_1");
    assert_eq!(input[2]["name"], "read_file");
    assert_eq!(
        input[2]["arguments"],
        json!({"path": "src/main.rs"}).to_string()
    );

    // Tool function_call_output success
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_read_1");
    assert_eq!(input[3]["output"], "fn main() {}");

    // Tool function_call_output error
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "call_failed_2");
    assert_eq!(input[4]["output"], "[Error] Permission denied");
}

#[test]
fn test_build_body_tools_responses_format() {
    let provider = make_test_provider(Some(CodexAuth::api_key("test-key")), None);

    let req = CompletionRequest {
        model: "custom-plain".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![ToolDefinition {
            name: "grep".to_string(),
            description: "Search pattern".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "pattern": { "type": "string" } },
                "required": ["pattern"]
            }),
        }],
        effort: None,
        max_output_tokens: None,
    };

    let body = provider.build_body(&req);

    assert_eq!(body["tool_choice"], "auto");
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);

    // Responses format: name, description, parameters are direct fields of the tool object
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "grep");
    assert_eq!(tools[0]["description"], "Search pattern");
    assert_eq!(
        tools[0]["parameters"],
        json!({
            "type": "object",
            "properties": { "pattern": { "type": "string" } },
            "required": ["pattern"]
        })
    );
    assert!(tools[0].get("function").is_none());
}

#[test]
fn test_build_body_reasoning_block() {
    let mut effort_map = BTreeMap::new();
    effort_map.insert("high".to_string(), "xhigh".to_string());

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![ModelConfig {
            id: "gpt-5.1-codex".to_string(),
            display_name: None,
            context_window: Some(128_000),
            reasoning: Some(true),
        }],
        catalog: false,
        context_window: None,
        reasoning: None,
        effort_map,
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = Codex::new(
        "codex",
        cfg,
        ProviderDefaults::default(),
        Some(CodexAuth::api_key("test")),
        BTreeMap::new(),
    )
    .expect("valid");

    // 1. Model with reasoning -> sends {"effort": "...", "summary": "auto"}
    let req_reasoning = CompletionRequest {
        model: "gpt-5.1-codex".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: Some(Effort::Medium),
        max_output_tokens: None,
    };
    let body1 = provider.build_body(&req_reasoning);
    assert_eq!(
        body1["reasoning"],
        json!({
            "effort": "medium",
            "summary": "auto"
        })
    );

    // 2. High mapped via effort_map
    let req_high = CompletionRequest {
        effort: Some(Effort::High),
        ..req_reasoning.clone()
    };
    let body2 = provider.build_body(&req_high);
    assert_eq!(body2["reasoning"]["effort"], "xhigh");
    assert_eq!(body2["reasoning"]["summary"], "auto");

    // 3. Non-reasoning model -> no reasoning field
    let req_plain = CompletionRequest {
        model: "llama-3.1".to_string(),
        ..req_reasoning
    };
    let body3 = provider.build_body(&req_plain);
    assert!(body3.get("reasoning").is_none());
}

// ---------------------------------------------------------------------------
// 4. Responses SSE Stream Decoding
// ---------------------------------------------------------------------------

#[test]
fn test_sse_decode_text_and_reasoning_delta() {
    let mut state = ResponsesChunkState::new();

    // 1. Output text delta
    let text_chunk = json!({
        "type": "response.output_text.delta",
        "delta": "Hello, world!"
    });
    let events = responses_chunk_to_events(&text_chunk, &mut state).expect("ok");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::TextDelta {
            text: "Hello, world!".to_string()
        }
    );

    // 2. Reasoning summary delta
    let reasoning_chunk = json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": "Thinking step 1"
    });
    let events2 = responses_chunk_to_events(&reasoning_chunk, &mut state).expect("ok");
    assert_eq!(events2.len(), 1);
    assert_eq!(
        events2[0],
        StreamEvent::ReasoningDelta {
            text: "Thinking step 1".to_string()
        }
    );

    // 3. Alternate reasoning delta type
    let reasoning_chunk2 = json!({
        "type": "response.reasoning.delta",
        "delta": " step 2"
    });
    let events3 = responses_chunk_to_events(&reasoning_chunk2, &mut state).expect("ok");
    assert_eq!(events3.len(), 1);
    assert_eq!(
        events3[0],
        StreamEvent::ReasoningDelta {
            text: " step 2".to_string()
        }
    );
}

#[test]
fn test_sse_decode_function_call_streaming_sequence() {
    let mut state = ResponsesChunkState::new();

    // 1. Output item added (starts tool call)
    let added = json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {
            "type": "function_call",
            "id": "fc_item_0",
            "call_id": "call_abc123",
            "name": "bash",
            "arguments": ""
        }
    });
    let ev1 = responses_chunk_to_events(&added, &mut state).expect("ok");
    assert_eq!(ev1.len(), 1);
    assert_eq!(
        ev1[0],
        StreamEvent::ToolCallStart {
            index: 0,
            id: CallId::new("call_abc123"),
            name: "bash".to_string(),
        }
    );
    assert!(state.any_tool_call());

    // 2. Argument deltas
    let delta1 = json!({
        "type": "response.function_call_arguments.delta",
        "item_id": "fc_item_0",
        "delta": "{\"cmd\": "
    });
    let ev2 = responses_chunk_to_events(&delta1, &mut state).expect("ok");
    assert_eq!(ev2.len(), 1);
    assert_eq!(
        ev2[0],
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "{\"cmd\": ".to_string(),
        }
    );

    let delta2 = json!({
        "type": "response.function_call_arguments.delta",
        "item_id": "fc_item_0",
        "delta": "\"ls\"}"
    });
    let ev3 = responses_chunk_to_events(&delta2, &mut state).expect("ok");
    assert_eq!(ev3.len(), 1);
    assert_eq!(
        ev3[0],
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "\"ls\"}".to_string(),
        }
    );

    // 3. Arguments done
    let done = json!({
        "type": "response.function_call_arguments.done",
        "item_id": "fc_item_0",
        "arguments": "{\"cmd\": \"ls\"}"
    });
    let ev4 = responses_chunk_to_events(&done, &mut state).expect("ok");
    assert!(ev4.is_empty());

    // 4. Output item done (should not duplicate)
    let item_done = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "id": "fc_item_0",
            "call_id": "call_abc123",
            "name": "bash",
            "arguments": "{\"cmd\": \"ls\"}"
        }
    });
    let ev5 = responses_chunk_to_events(&item_done, &mut state).expect("ok");
    assert!(ev5.is_empty());
}

#[test]
fn test_sse_decode_function_call_done_only() {
    let mut state = ResponsesChunkState::new();

    // Output item done arriving directly (non-streaming or batch)
    let item_done = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "id": "fc_item_1",
            "call_id": "call_xyz789",
            "name": "read_file",
            "arguments": "{\"path\":\"foo.rs\"}"
        }
    });

    let ev = responses_chunk_to_events(&item_done, &mut state).expect("ok");
    assert_eq!(ev.len(), 2);
    assert_eq!(
        ev[0],
        StreamEvent::ToolCallStart {
            index: 0,
            id: CallId::new("call_xyz789"),
            name: "read_file".to_string(),
        }
    );
    assert_eq!(
        ev[1],
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "{\"path\":\"foo.rs\"}".to_string(),
        }
    );
    assert!(state.any_tool_call());
}

#[test]
fn test_sse_decode_completed_with_usage() {
    let mut state = ResponsesChunkState::new();

    let completed = json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 120,
                "output_tokens": 45,
                "input_tokens_details": {
                    "cached_tokens": 30
                },
                "output_tokens_details": {
                    "reasoning_tokens": 15
                }
            }
        }
    });

    let events = responses_chunk_to_events(&completed, &mut state).expect("ok");
    assert_eq!(events.len(), 2);

    assert_eq!(
        events[0],
        StreamEvent::Usage(Usage {
            input_tokens: 120,
            output_tokens: 45,
            reasoning_tokens: 15,
            cache_read_tokens: 30,
            cache_write_tokens: 0,
        })
    );
    assert_eq!(
        events[1],
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );
    assert!(state.message_end_emitted());

    // If tool calls were made, stop reason is ToolUse
    let mut tool_state = ResponsesChunkState::new();
    tool_state.any_tool_call = true;
    let tool_events = responses_chunk_to_events(&completed, &mut tool_state).expect("ok");
    assert_eq!(
        tool_events[1],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );
}

// ---------------------------------------------------------------------------
// 5. Error Mapping
// ---------------------------------------------------------------------------

#[test]
fn test_error_mapping_in_stream() {
    // 1. 401 Auth error
    let auth_val = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "code": 401,
                "message": "Incorrect API key provided"
            }
        }
    });
    let err = extract_responses_error(&auth_val);
    assert!(matches!(err, ProviderError::Auth(_)));

    // 2. 429 Rate limit error
    let rate_val = json!({
        "type": "error",
        "error": {
            "code": "rate_limit_exceeded",
            "message": "Rate limit reached for requests"
        }
    });
    let err2 = extract_responses_error(&rate_val);
    assert!(matches!(err2, ProviderError::RateLimited(_)));
    assert!(err2.is_retryable());

    // 3. 500 Server error
    let server_val = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "code": 500,
                "message": "Internal server error"
            }
        }
    });
    let err3 = extract_responses_error(&server_val);
    match err3 {
        ProviderError::Http { status, .. } => assert_eq!(status, 500),
        other => panic!("expected Http, got {other:?}"),
    }
    assert!(err3.is_retryable());
}

// ---------------------------------------------------------------------------
// 6. End-to-End Mocking Style Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complete_e2e_streaming_success() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]);

        // Verify headers sent by provider
        assert!(request_str.contains("POST /responses HTTP/1.1"));
        assert!(request_str.contains("authorization: Bearer my-key"));
        assert!(request_str.contains("openai-beta: responses=experimental"));

        let sse_body = [
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" there!\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}}\n\n",
            "data: [DONE]\n\n",
        ].join("");

        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            sse_body.len(),
            sse_body
        );
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
    };

    let provider = Codex::new(
        "test-codex",
        cfg,
        ProviderDefaults::default(),
        Some(CodexAuth::api_key("my-key")),
        BTreeMap::new(),
    )
    .expect("valid");

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "You are a helper".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete ok");
    server.await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream item ok"));
    }

    // MessageStart is emitted before first TextDelta
    assert!(matches!(&events[0], StreamEvent::MessageStart { .. }));
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            text: "Hi".to_string()
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            text: " there!".to_string()
        }
    );
    assert!(matches!(&events[3], StreamEvent::Usage(_)));
    assert!(matches!(&events[4], StreamEvent::MessageEnd { .. }));
}

#[tokio::test]
async fn test_complete_e2e_401_auth_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let body = json!({
            "error": { "code": "invalid_api_key", "message": "Incorrect API key" }
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        models: vec![],
        catalog: false,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = Codex::new(
        "test-codex",
        cfg,
        ProviderDefaults::default(),
        Some(CodexAuth::api_key("bad-key")),
        BTreeMap::new(),
    )
    .expect("valid")
    .with_backoff_base(Duration::from_millis(1));

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let result = provider.complete(req).await;
    server.await.unwrap();

    match result {
        Err(ProviderError::Auth(msg)) => {
            assert!(msg.contains("401"));
            assert!(msg.contains("Incorrect API key"));
        }
        Err(other) => panic!("expected ProviderError::Auth, got {other:?}"),
        Ok(_) => panic!("expected ProviderError::Auth, got Ok"),
    }
}

#[tokio::test]
async fn test_complete_e2e_429_rate_limit_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Accept until attempts are exhausted
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = json!({
                "error": { "code": "rate_limit_exceeded", "message": "Too many requests" }
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
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
    };

    let defaults = ProviderDefaults {
        max_retries: 1,
        ..ProviderDefaults::default()
    };

    let provider = Codex::new(
        "test-codex",
        cfg,
        defaults,
        Some(CodexAuth::api_key("key")),
        BTreeMap::new(),
    )
    .expect("valid")
    .with_backoff_base(Duration::from_millis(1));

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let result = provider.complete(req).await;
    server.abort();

    match result {
        Err(ProviderError::RateLimited(msg)) => {
            assert!(msg.contains("Too many requests"));
        }
        Err(other) => panic!("expected ProviderError::RateLimited, got {other:?}"),
        Ok(_) => panic!("expected ProviderError::RateLimited, got Ok"),
    }
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
        assert!(req_str.contains("authorization: Bearer my-key"));

        // Codex backend format has "models" with "slug" or "id"
        let body = json!({
            "models": [
                { "slug": "gpt-5.1-codex", "display_name": "GPT-5.1 Codex" },
                { "slug": "gpt-5.1-codex-mini", "display_name": "GPT-5.1 Codex Mini" },
                { "id": "text-embedding-3-small" }
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
            id: "gpt-5.1-codex".to_string(),
            display_name: Some("GPT-5.1 Custom Name".to_string()),
            context_window: Some(128_000),
            reasoning: Some(true),
        }],
        catalog: true,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = Codex::new(
        "test-codex",
        cfg,
        ProviderDefaults::default(),
        Some(CodexAuth::api_key("my-key")),
        BTreeMap::new(),
    )
    .expect("valid");

    let models = provider.list_models().await.expect("models ok");
    server.await.unwrap();

    assert_eq!(models.len(), 3);
    assert_eq!(models[0].route.model, "gpt-5.1-codex");
    assert_eq!(models[0].display_name, "GPT-5.1 Custom Name");
    assert!(models[0].supports_reasoning);
    assert_eq!(models[1].route.model, "gpt-5.1-codex-mini");
    assert!(models[1].supports_reasoning);
    assert_eq!(models[2].route.model, "text-embedding-3-small");

    // Second call tests cache (server already closed)
    let cached = provider.list_models().await.expect("cached ok");
    assert_eq!(cached.len(), 3);
}

#[test]
fn test_model_info_pricing_and_reasoning() {
    let mut pricing = BTreeMap::new();
    pricing.insert(
        "gpt-5.1-codex".to_string(),
        pacode_types::Pricing {
            input_per_m: 2.0,
            output_per_m: 8.0,
            cache_read_per_m: Some(1.0),
            cache_write_per_m: None,
        },
    );

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![ModelConfig {
            id: "gpt-5.1-codex".to_string(),
            display_name: Some("Custom Name".to_string()),
            context_window: Some(128_000),
            reasoning: Some(true),
        }],
        catalog: false,
        context_window: Some(64_000),
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = Codex::new(
        "codex",
        cfg,
        ProviderDefaults::default(),
        Some(CodexAuth::api_key("key")),
        pricing,
    )
    .expect("valid");

    let info = provider.model_info("gpt-5.1-codex");
    assert_eq!(info.display_name, "Custom Name");
    assert_eq!(info.context_window, Some(128_000));
    assert!(info.supports_reasoning);
    assert_eq!(info.pricing.unwrap().input_per_m, 2.0);

    // Unconfigured reasoning model
    let info2 = provider.model_info("o3-mini");
    assert!(info2.supports_reasoning);
    assert_eq!(info2.context_window, Some(64_000));

    // Unconfigured non-reasoning model
    let info3 = provider.model_info("gpt-4o");
    assert!(!info3.supports_reasoning);
}
