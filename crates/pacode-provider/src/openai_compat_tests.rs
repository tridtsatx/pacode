use std::collections::BTreeMap;

use pacode_types::{
    CallId, ContentBlock, Effort, Message, ModelConfig, ProviderConfig, ProviderDefaults, Role,
    ToolDefinition,
};
use serde_json::json;

use crate::{CompletionRequest, OpenAiCompat, Provider, ProviderError, redact};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn make_provider(
    models: Vec<ModelConfig>,
    reasoning: Option<bool>,
    effort_map: BTreeMap<String, String>,
    extra_body: Option<serde_json::Value>,
) -> OpenAiCompat {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models,
        catalog: true,
        context_window: Some(128_000),
        reasoning,
        effort_map,
        extra_body,
        headers: BTreeMap::new(),
    };
    OpenAiCompat::new(
        "openai",
        cfg,
        ProviderDefaults::default(),
        Some("test-key".to_string()),
        BTreeMap::new(),
    )
    .expect("valid provider")
}

#[test]
fn test_build_body_system_merge() {
    let provider = make_provider(vec![], None, BTreeMap::new(), None);

    // 1. Both static and dynamic
    let req1 = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "Static prompt".to_string(),
        system_dynamic: "Dynamic prompt".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };
    let body1 = provider.build_body(&req1);
    assert_eq!(
        body1["messages"][0]["content"],
        "Static prompt\n\nDynamic prompt"
    );
    assert_eq!(body1["messages"][0]["role"], "system");

    // 2. Static only (dynamic empty -> skipped)
    let req2 = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "Static prompt".to_string(),
        system_dynamic: String::new(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };
    let body2 = provider.build_body(&req2);
    assert_eq!(body2["messages"][0]["content"], "Static prompt");

    // 3. Dynamic only
    let req3 = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: String::new(),
        system_dynamic: "Dynamic prompt".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };
    let body3 = provider.build_body(&req3);
    assert_eq!(body3["messages"][0]["content"], "Dynamic prompt");
}

#[test]
fn test_build_body_messages_conversion() {
    let provider = make_provider(vec![], None, BTreeMap::new(), None);

    let call_id = CallId::new("call_abc123");
    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "System".to_string(),
        system_dynamic: String::new(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "Hello".to_string(),
                    },
                    ContentBlock::Reasoning {
                        text: "Dropped reasoning".to_string(),
                        signature: None,
                    },
                ],
                meta: pacode_types::MessageMeta::default(),
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Thinking done".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: "read_file".to_string(),
                        input: json!({"path": "src/main.rs"}),
                    },
                ],
                meta: pacode_types::MessageMeta::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: call_id.clone(),
                    content: "file content here".to_string(),
                    is_error: false,
                }],
                meta: pacode_types::MessageMeta::default(),
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: CallId::new("call_xyz"),
                    name: "list_dir".to_string(),
                    input: json!({"path": "."}),
                }],
                meta: pacode_types::MessageMeta::default(),
            },
        ],
        tools: vec![],
        effort: None,
        max_output_tokens: Some(4096),
    };

    let body = provider.build_body(&req);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 5); // 1 system + 4 req

    // User message
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "Hello");

    // Assistant message with text and tool call
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["content"], "Thinking done");
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_abc123");
    assert_eq!(msgs[2]["tool_calls"][0]["type"], "function");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(
        msgs[2]["tool_calls"][0]["function"]["arguments"],
        json!({"path": "src/main.rs"}).to_string()
    );

    // Tool result message
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_abc123");
    assert_eq!(msgs[3]["content"], "file content here");

    // Assistant with tool call only -> content is null
    assert_eq!(msgs[4]["role"], "assistant");
    assert!(msgs[4]["content"].is_null());
    assert_eq!(msgs[4]["tool_calls"][0]["id"], "call_xyz");

    // max_tokens
    assert_eq!(body["max_tokens"], 4096);
}

#[test]
fn test_build_body_tools_shape() {
    let provider = make_provider(vec![], None, BTreeMap::new(), None);

    // With tools
    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "Sys".to_string(),
        system_dynamic: String::new(),
        messages: vec![],
        tools: vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get current weather".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"]
            }),
        }],
        effort: None,
        max_output_tokens: None,
    };
    let body = provider.build_body(&req);
    assert_eq!(body["tool_choice"], "auto");
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "get_weather");
    assert_eq!(tools[0]["function"]["description"], "Get current weather");
    assert_eq!(
        tools[0]["function"]["parameters"]["required"],
        json!(["location"])
    );

    // Without tools
    let req_no_tools = CompletionRequest {
        tools: vec![],
        ..req
    };
    let body_no_tools = provider.build_body(&req_no_tools);
    assert!(body_no_tools.get("tools").is_none());
    assert!(body_no_tools.get("tool_choice").is_none());
}

#[test]
fn test_build_body_reasoning_effort_and_map() {
    let mut effort_map = BTreeMap::new();
    effort_map.insert("high".to_string(), "extreme".to_string());
    effort_map.insert("max".to_string(), "maximum".to_string());

    let provider = make_provider(
        vec![ModelConfig {
            id: "custom-reasoning".to_string(),
            display_name: None,
            context_window: None,
            reasoning: Some(true),
        }],
        None,
        effort_map,
        None,
    );

    // 1. Model heuristic enables reasoning (e.g. o3-mini)
    let req_o3 = CompletionRequest {
        model: "o3-mini".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: Some(Effort::Medium),
        max_output_tokens: None,
    };
    let body_o3 = provider.build_body(&req_o3);
    assert_eq!(body_o3["reasoning_effort"], "medium");

    // 2. High mapped with effort_map override
    let req_high = CompletionRequest {
        effort: Some(Effort::High),
        ..req_o3.clone()
    };
    let body_high = provider.build_body(&req_high);
    assert_eq!(body_high["reasoning_effort"], "extreme");

    // 3. Max mapped with effort_map override
    let req_max = CompletionRequest {
        effort: Some(Effort::Max),
        ..req_o3.clone()
    };
    let body_max = provider.build_body(&req_max);
    assert_eq!(body_max["reasoning_effort"], "maximum");

    // 4. Model config reasoning = true
    let req_custom = CompletionRequest {
        model: "custom-reasoning".to_string(),
        effort: Some(Effort::Low),
        ..req_o3.clone()
    };
    let body_custom = provider.build_body(&req_custom);
    assert_eq!(body_custom["reasoning_effort"], "low");

    // 5. Non-reasoning model (gpt-4o) without heuristic match
    let req_gpt4o = CompletionRequest {
        model: "gpt-4o".to_string(),
        effort: Some(Effort::High),
        ..req_o3.clone()
    };
    let body_gpt4o = provider.build_body(&req_gpt4o);
    assert!(body_gpt4o.get("reasoning_effort").is_none());
}

#[test]
fn test_build_body_extra_body_merge() {
    let extra = json!({
        "temperature": 0.7,
        "stream_options": {
            "custom_flag": true
        },
        "chat_template_kwargs": {
            "thinking": true
        }
    });

    let provider = make_provider(vec![], None, BTreeMap::new(), Some(extra));

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "Static".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let body = provider.build_body(&req);
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["stream_options"]["custom_flag"], true);
    assert_eq!(body["chat_template_kwargs"]["thinking"], true);
}

#[test]
fn test_model_info() {
    let mut pricing = BTreeMap::new();
    pricing.insert(
        "custom-model".to_string(),
        pacode_types::Pricing {
            input_per_m: 2.5,
            output_per_m: 10.0,
            cache_read_per_m: Some(1.25),
            cache_write_per_m: None,
        },
    );

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![ModelConfig {
            id: "custom-model".to_string(),
            display_name: Some("Custom Display".to_string()),
            context_window: Some(64_000),
            reasoning: Some(true),
        }],
        catalog: false,
        context_window: Some(128_000),
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = OpenAiCompat::new("my-openai", cfg, ProviderDefaults::default(), None, pricing)
        .expect("ok");

    // 1. Configured model
    let info = provider.model_info("custom-model");
    assert_eq!(info.route.provider, "my-openai");
    assert_eq!(info.route.model, "custom-model");
    assert_eq!(info.display_name, "Custom Display");
    assert_eq!(info.context_window, Some(64_000));
    assert!(info.supports_reasoning);
    assert_eq!(info.pricing.unwrap().input_per_m, 2.5);

    // 2. Unconfigured model with heuristic reasoning (e.g. deepseek-r1)
    let info2 = provider.model_info("deepseek-r1-distill");
    assert_eq!(info2.display_name, "Deepseek R1 Distill");
    assert_eq!(info2.context_window, Some(128_000));
    assert!(info2.supports_reasoning);
    assert!(info2.pricing.is_none());

    // 3. Unconfigured model without reasoning (e.g. llama-3.1-8b)
    let info3 = provider.model_info("llama-3.1-8b");
    assert!(!info3.supports_reasoning);
}

#[tokio::test]
async fn test_list_models_catalog_false() {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![
            ModelConfig {
                id: "gpt-4o".to_string(),
                display_name: None,
                context_window: None,
                reasoning: None,
            },
            ModelConfig {
                id: "o3-mini".to_string(),
                display_name: None,
                context_window: None,
                reasoning: None,
            },
        ],
        catalog: false,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = OpenAiCompat::new(
        "openai",
        cfg,
        ProviderDefaults::default(),
        None,
        BTreeMap::new(),
    )
    .expect("ok");

    let models = provider.list_models().await.expect("ok");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].route.model, "gpt-4o");
    assert_eq!(models[1].route.model, "o3-mini");
}

#[tokio::test]
async fn test_list_models_catalog_fetch_and_merge() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let body = json!({
            "data": [
                { "id": "gpt-4o" },
                { "id": "gpt-4o-mini" },
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
            id: "gpt-4o".to_string(),
            display_name: Some("GPT-4o Configured".to_string()),
            context_window: Some(128_000),
            reasoning: Some(false),
        }],
        catalog: true,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = OpenAiCompat::new(
        "test-prov",
        cfg,
        ProviderDefaults::default(),
        None,
        BTreeMap::new(),
    )
    .expect("ok");

    let models = provider.list_models().await.expect("ok");
    server.await.unwrap();

    assert_eq!(models.len(), 3);
    assert_eq!(models[0].route.model, "gpt-4o");
    assert_eq!(models[0].display_name, "GPT-4o Configured");
    assert_eq!(models[1].route.model, "gpt-4o-mini");
    assert_eq!(models[2].route.model, "text-embedding-3-small");

    // Second call tests cache (server already closed)
    let cached = provider.list_models().await.expect("cached ok");
    assert_eq!(cached.len(), 3);
}

#[test]
fn test_redact_bearer_token() {
    assert_eq!(redact("Bearer sk-test12345"), "Bearer [REDACTED]");
    assert_eq!(redact("bearer abcdef"), "bearer [REDACTED]");
    assert_eq!(redact("BEARER XYZ"), "BEARER [REDACTED]");
    assert_eq!(redact("Bearer: my-token"), "Bearer: [REDACTED]");
    assert_eq!(redact("\"Bearer sk-12345\""), "\"Bearer [REDACTED]\"");
    assert_eq!(
        redact("Bearer token1 and Bearer token2"),
        "Bearer [REDACTED] and Bearer [REDACTED]"
    );
}

#[test]
fn test_redact_api_key() {
    assert_eq!(redact("api_key=mysecret"), "api_key=[REDACTED]");
    assert_eq!(
        redact("api_key=mysecret&model=gpt-4"),
        "api_key=[REDACTED]&model=gpt-4"
    );
    assert_eq!(
        redact("{\"api_key\": \"mysecret\"}"),
        "{\"api_key\": \"[REDACTED]\"}"
    );
    assert_eq!(
        redact("{\"api_key\":\"mysecret\"}"),
        "{\"api_key\":\"[REDACTED]\"}"
    );
    assert_eq!(redact("api_key: mysecret"), "api_key: [REDACTED]");
    assert_eq!(redact("apikey=mysecret"), "apikey=[REDACTED]");
    assert_eq!(redact("api-key: mysecret"), "api-key: [REDACTED]");
}

#[test]
fn test_redact_authorization() {
    assert_eq!(
        redact("Authorization: Bearer secret123"),
        "Authorization: Bearer [REDACTED]"
    );
    assert_eq!(
        redact("{\"authorization\": \"Bearer secret123\"}"),
        "{\"authorization\": \"Bearer [REDACTED]\"}"
    );
    assert_eq!(
        redact("Authorization: secret123"),
        "Authorization: [REDACTED]"
    );
    assert_eq!(
        redact("authorization=secret123"),
        "authorization=[REDACTED]"
    );
}

#[test]
fn test_redact_no_sensitive_data() {
    let safe = "POST https://api.openai.com/v1/chat/completions model=gpt-4o keys=[\"messages\", \"model\"]";
    assert_eq!(redact(safe), safe);
}

#[tokio::test]
async fn test_complete_non_2xx_carries_full_error_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let error_body = json!({
            "error": {
                "code": "invalid_request_error",
                "message": "Invalid model specified: foo"
            }
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            error_body.len(),
            error_body
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: format!("http://127.0.0.1:{port}"),
        api_key: Some("test-key".to_string()),
        api_key_env: None,
        models: vec![],
        catalog: false,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = OpenAiCompat::new(
        "test-prov",
        cfg,
        ProviderDefaults::default(),
        Some("test-key".to_string()),
        BTreeMap::new(),
    )
    .expect("ok");

    let req = CompletionRequest {
        model: "invalid-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let result = provider.complete(req).await;
    server.await.unwrap();

    let expected_body = "{\"error\":{\"code\":\"invalid_request_error\",\"message\":\"Invalid model specified: foo\"}}";
    match result {
        Err(ProviderError::Http { status, message }) => {
            assert_eq!(status, 400);
            assert_eq!(message, expected_body);
        }
        Err(other) => panic!("expected ProviderError::Http, got: {other}"),
        Ok(_) => panic!("expected ProviderError::Http, got a successful stream"),
    }

    let err: ProviderError = ProviderError::Http {
        status: 400,
        message: expected_body.to_string(),
    };
    let err_str = format!("{err}");
    assert_eq!(
        err_str,
        "request failed (400): {\"error\":{\"code\":\"invalid_request_error\",\"message\":\"Invalid model specified: foo\"}}"
    );
}
