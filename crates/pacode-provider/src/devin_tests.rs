use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{
    Effort, Message, ModelConfig, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::devin::Devin;
use crate::devin::connect::{
    CONNECT_FLAG_END_STREAM, ConnectClient, ConnectFrameDecoder, encode_connect_frame,
};
use crate::{CompletionRequest, Provider, ProviderError};

fn make_test_provider(base_url: &str, api_key: Option<&str>) -> Devin {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: base_url.to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![],
        catalog: true,
        context_window: Some(128_000),
        reasoning: Some(true),
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let defaults = ProviderDefaults {
        max_retries: 2,
        stream_idle_secs: 2,
        ..ProviderDefaults::default()
    };

    Devin::new(
        "devin-test",
        cfg,
        defaults,
        api_key.unwrap_or("test-api-key"),
        base_url,
        BTreeMap::new(),
    )
    .expect("valid provider")
    .with_backoff_base(Duration::from_millis(2))
}

// ---------------------------------------------------------------------------
// 1. Connect Unary Request Shape and Headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_connect_unary_request_shape_and_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request_str.contains("POST /exa.api_server_pb.ApiServerService/TestUnary HTTP/1.1")
        );
        assert!(request_str.contains("content-type: application/json"));
        assert!(request_str.contains("connect-protocol-version: 1"));
        assert!(request_str.contains("authorization: Bearer my-secret-key"));
        assert!(request_str.contains("\"query\":\"ping\""));

        let resp_body = json!({"result": "pong"}).to_string();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        socket.write_all(http_resp.as_bytes()).await.unwrap();
    });

    let client = reqwest::Client::new();
    let connect = ConnectClient::new(
        client,
        format!("http://127.0.0.1:{port}"),
        "my-secret-key",
        vec![],
    );

    let req_body = json!({"query": "ping"});
    let resp: serde_json::Value = connect
        .unary("/exa.api_server_pb.ApiServerService/TestUnary", &req_body)
        .await
        .expect("unary call succeeds");

    server.await.unwrap();
    assert_eq!(resp["result"], "pong");
}

// ---------------------------------------------------------------------------
// 2. Connect Streaming Frame Decoder (Data + Trailers With/Without Error)
// ---------------------------------------------------------------------------

#[test]
fn test_connect_streaming_frame_decoder_data_and_trailers() {
    let mut decoder = ConnectFrameDecoder::new();

    // 1. Data frame
    let data_payload = json!({"delta_text": "Hello world"}).to_string();
    let frame1 = encode_connect_frame(0x00, data_payload.as_bytes());
    let decoded = decoder.feed(&frame1).expect("feed succeeds");
    assert_eq!(decoded.len(), 1);
    assert!(decoded[0].is_data());
    assert_eq!(decoded[0].data().unwrap()["delta_text"], "Hello world");

    // 2. Trailer frame without error
    let trailer_clean = json!({}).to_string();
    let frame2 = encode_connect_frame(CONNECT_FLAG_END_STREAM, trailer_clean.as_bytes());
    let decoded2 = decoder.feed(&frame2).expect("feed succeeds");
    assert_eq!(decoded2.len(), 1);
    assert!(decoded2[0].is_end());
    assert!(decoded2[0].end_error().is_none());

    // 3. Trailer frame with error
    let trailer_err = json!({
        "error": {
            "code": "resource_exhausted",
            "message": "Quota limit reached"
        }
    })
    .to_string();
    let frame3 = encode_connect_frame(CONNECT_FLAG_END_STREAM, trailer_err.as_bytes());
    let decoded3 = decoder.feed(&frame3).expect("feed succeeds");
    assert_eq!(decoded3.len(), 1);
    assert!(decoded3[0].is_end());
    let err = decoded3[0].end_error().expect("trailer error present");
    match err {
        ProviderError::RateLimited(msg) => assert!(msg.contains("Quota limit reached")),
        other => panic!("expected RateLimited, got {other:?}"),
    }

    // 4. Fragmented feed (feed byte-by-byte across header and payload)
    let data_payload2 = json!({"delta_thinking": "Thinking step"}).to_string();
    let frame4 = encode_connect_frame(0x00, data_payload2.as_bytes());
    assert!(decoder.feed(&frame4[..2]).unwrap().is_empty());
    assert!(decoder.feed(&frame4[2..5]).unwrap().is_empty());
    assert!(decoder.feed(&frame4[5..10]).unwrap().is_empty());
    let final_decoded = decoder.feed(&frame4[10..]).unwrap();
    assert_eq!(final_decoded.len(), 1);
    assert_eq!(
        final_decoded[0].data().unwrap()["delta_thinking"],
        "Thinking step"
    );

    // 5. Multiple frames in a single chunk
    let mut multi_buf = Vec::new();
    multi_buf.extend(encode_connect_frame(0x00, b"{\"delta_text\":\"A\"}"));
    multi_buf.extend(encode_connect_frame(0x00, b"{\"delta_text\":\"B\"}"));
    multi_buf.extend(encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"));
    let multi_decoded = decoder.feed(&multi_buf).expect("multi frame ok");
    assert_eq!(multi_decoded.len(), 3);
    assert_eq!(multi_decoded[0].data().unwrap()["delta_text"], "A");
    assert_eq!(multi_decoded[1].data().unwrap()["delta_text"], "B");
    assert!(multi_decoded[2].is_end());

    // 6. Incomplete finish error
    let mut incomplete_decoder = ConnectFrameDecoder::new();
    incomplete_decoder.feed(&[0x00, 0x00, 0x00]).unwrap();
    assert!(incomplete_decoder.finish().is_err());
}

#[test]
fn test_connect_streaming_frame_decoder_malformed_json() {
    let mut decoder = ConnectFrameDecoder::new();
    let malformed_frame = encode_connect_frame(0x00, b"not-valid-json");
    let res = decoder.feed(&malformed_frame);
    assert!(matches!(res, Err(ProviderError::Malformed(_))));

    let mut trailer_decoder = ConnectFrameDecoder::new();
    let malformed_trailer = encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{bad json");
    let res_trailer = trailer_decoder.feed(&malformed_trailer);
    assert!(matches!(res_trailer, Err(ProviderError::Malformed(_))));
}

// ---------------------------------------------------------------------------
// 3. list_models Mapping and Merge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_list_models_mapping_and_merge() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request_str.contains("POST /exa.api_server_pb.ApiServerService/GetCliModelConfigs")
        );
        assert!(request_str.contains("content-type: application/json"));
        assert!(request_str.contains("connect-protocol-version: 1"));

        let resp_obj = json!({
            "client_model_configs": [
                {
                    "model_uid": "devin-model-1",
                    "display_name": "Devin Fast",
                    "context_window": 128000,
                    "supports_reasoning": true
                },
                {
                    "model_uid": "devin-custom-1",
                    "display_name": "Server Overwrite Ignored"
                }
            ],
            "subagent_default_model_uid": "devin-subagent-v1",
            "default_override_model_config": {
                "model_uid": "devin-override-model",
                "display_name": "Devin Override Model",
                "context_window": 256000,
                "supports_reasoning": true
            }
        });

        let resp_body = resp_obj.to_string();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        socket.write_all(http_resp.as_bytes()).await.unwrap();
    });

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: format!("http://127.0.0.1:{port}"),
        api_key: None,
        api_key_env: None,
        models: vec![ModelConfig {
            id: "devin-custom-1".to_string(),
            display_name: Some("Devin Custom Name".to_string()),
            context_window: Some(64_000),
            reasoning: Some(true),
        }],
        catalog: true,
        context_window: Some(128_000),
        reasoning: Some(true),
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let provider = Devin::new(
        "devin-test",
        cfg,
        ProviderDefaults::default(),
        "my-key",
        format!("http://127.0.0.1:{port}"),
        BTreeMap::new(),
    )
    .expect("valid provider");
    let models = provider.list_models().await.expect("list_models succeeds");
    server.await.unwrap();

    // 1. Configured model preserved first with its local settings
    assert_eq!(models[0].route.model, "devin-custom-1");
    assert_eq!(models[0].display_name, "Devin Custom Name");
    assert_eq!(models[0].context_window, Some(64_000));

    // 2. Remote models merged in without duplication
    let ids: Vec<&str> = models.iter().map(|m| m.route.model.as_str()).collect();
    assert!(ids.contains(&"devin-model-1"));
    assert!(ids.contains(&"devin-subagent-v1"));
    assert!(ids.contains(&"devin-override-model"));
    assert_eq!(ids.len(), 4);

    let devin_fast = models
        .iter()
        .find(|m| m.route.model == "devin-model-1")
        .unwrap();
    assert_eq!(devin_fast.display_name, "Devin Fast");
    assert_eq!(devin_fast.context_window, Some(128_000));
    assert!(devin_fast.supports_reasoning);

    // 3. Second call hits cache (mock server has exited)
    let cached = provider.list_models().await.expect("cached models");
    assert_eq!(cached.len(), 4);
}

// ---------------------------------------------------------------------------
// 4. AssignModel-then-GetChatMessage Sequence (Text + Thinking + Usage + Stop)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_assign_model_then_get_chat_message_sequence() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Step 1: AssignModel unary call
        let (mut socket1, _) = listener.accept().await.unwrap();
        let mut buf1 = [0u8; 2048];
        let n1 = socket1.read(&mut buf1).await.unwrap();
        let req1_str = String::from_utf8_lossy(&buf1[..n1]);

        assert!(req1_str.contains("POST /exa.api_server_pb.ApiServerService/AssignModel"));
        assert!(req1_str.contains("\"model_uid\":\"devin-fast-model\""));

        let assign_resp = json!({
            "assignment": {
                "model_uid": "devin-fast-model",
                "assignment_jwt": "jwt-token-for-devin-stream-12345",
                "harness_uids": ["harness-primary"]
            }
        })
        .to_string();

        let http_resp1 = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            assign_resp.len(),
            assign_resp
        );
        socket1.write_all(http_resp1.as_bytes()).await.unwrap();
        socket1.shutdown().await.unwrap();

        // Step 2: GetChatMessage streaming call
        let (mut socket2, _) = listener.accept().await.unwrap();
        let mut buf2 = [0u8; 4096];
        let n2 = socket2.read(&mut buf2).await.unwrap();
        let req2_str = String::from_utf8_lossy(&buf2[..n2]);

        assert!(req2_str.contains("POST /exa.api_server_pb.ApiServerService/GetChatMessage"));
        assert!(req2_str.contains("\"assignment_jwt\":\"jwt-token-for-devin-stream-12345\""));
        assert!(req2_str.contains("inference_request"));
        assert!(req2_str.contains("thinking"));

        // Send streaming Connect frames
        let header =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n";
        socket2.write_all(header.as_bytes()).await.unwrap();

        // Frame 1: Reasoning delta
        let f1_body = json!({
            "message_id": "msg_001",
            "delta_thinking": "Let me plan this step."
        })
        .to_string();
        socket2
            .write_all(&encode_connect_frame(0x00, f1_body.as_bytes()))
            .await
            .unwrap();

        // Frame 2: Text delta 1
        let f2_body = json!({
            "message_id": "msg_001",
            "delta_text": "Hello, "
        })
        .to_string();
        socket2
            .write_all(&encode_connect_frame(0x00, f2_body.as_bytes()))
            .await
            .unwrap();

        // Frame 3: Text delta 2
        let f3_body = json!({
            "message_id": "msg_001",
            "delta_text": "world!"
        })
        .to_string();
        socket2
            .write_all(&encode_connect_frame(0x00, f3_body.as_bytes()))
            .await
            .unwrap();

        // Frame 4: Usage
        let f4_body = json!({
            "message_id": "msg_001",
            "usage": {
                "input_tokens": 150,
                "output_tokens": 42,
                "reasoning_tokens": 20,
                "cache_read_tokens": 30,
                "cache_write_tokens": 10
            }
        })
        .to_string();
        socket2
            .write_all(&encode_connect_frame(0x00, f4_body.as_bytes()))
            .await
            .unwrap();

        // Frame 5: Stop reason
        let f5_body = json!({
            "message_id": "msg_001",
            "stop_reason": "end_turn"
        })
        .to_string();
        socket2
            .write_all(&encode_connect_frame(0x00, f5_body.as_bytes()))
            .await
            .unwrap();

        // Frame 6: Trailer without error
        socket2
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket2.shutdown().await.unwrap();
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("my-key"));
    let req = CompletionRequest {
        model: "devin-fast-model".to_string(),
        system_static: "System static".to_string(),
        system_dynamic: "System dynamic".to_string(),
        messages: vec![Message::user("Hi Devin")],
        tools: vec![],
        effort: Some(Effort::High),
        max_output_tokens: Some(1024),
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event ok"));
    }

    // Assert sequence: MessageStart -> ReasoningDelta -> TextDelta -> Usage -> MessageEnd
    assert!(matches!(&events[0], StreamEvent::MessageStart { .. }));
    assert_eq!(
        events[1],
        StreamEvent::ReasoningDelta {
            text: "Let me plan this step.".to_string()
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            text: "Hello, ".to_string()
        }
    );
    assert_eq!(
        events[3],
        StreamEvent::TextDelta {
            text: "world!".to_string()
        }
    );
    assert_eq!(
        events[4],
        StreamEvent::Usage(Usage {
            input_tokens: 150,
            output_tokens: 42,
            reasoning_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 10,
        })
    );
    assert_eq!(
        events[5],
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );
}

// ---------------------------------------------------------------------------
// 5. Error Mapping for unauthenticated, resource_exhausted, internal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_error_mapping_unauthenticated_unary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();

        let err_body = json!({
            "code": "unauthenticated",
            "message": "Invalid API key"
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            err_body.len(),
            err_body
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("bad-key"));
    let res = provider.list_models().await;
    server.await.unwrap();

    match res {
        Err(ProviderError::Auth(msg)) => assert!(msg.contains("Invalid API key")),
        other => panic!("expected ProviderError::Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_mapping_resource_exhausted_unary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Accept until attempts are exhausted
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            if socket.read(&mut buf).await.unwrap() == 0 {
                break;
            }
            let err_body = json!({
                "code": "resource_exhausted",
                "message": "Rate limit exceeded"
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                err_body.len(),
                err_body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("key"));
    let res = provider.list_models().await;
    server.abort();

    match res {
        Err(ProviderError::RateLimited(msg)) => assert!(msg.contains("Rate limit exceeded")),
        other => panic!("expected ProviderError::RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_mapping_internal_unary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            if socket.read(&mut buf).await.unwrap() == 0 {
                break;
            }
            let err_body = json!({
                "code": "internal",
                "message": "Database error"
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                err_body.len(),
                err_body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("key"));
    let res = provider.list_models().await;
    server.abort();

    match res {
        Err(ProviderError::Http { status, message }) => {
            assert_eq!(status, 500);
            assert!(message.contains("Database error"));
        }
        other => panic!("expected ProviderError::Http 500, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_mapping_streaming_trailer_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Step 1: AssignModel
        let (mut s1, _) = listener.accept().await.unwrap();
        let mut buf1 = [0u8; 2048];
        let _ = s1.read(&mut buf1).await.unwrap();
        let resp1 = json!({
            "assignment": {
                "model_uid": "devin-m1",
                "assignment_jwt": "jwt_tok_1"
            }
        })
        .to_string();
        s1.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                resp1.len(),
                resp1
            )
            .as_bytes(),
        )
        .await
        .unwrap();

        // Step 2: GetChatMessage with trailer error
        let (mut s2, _) = listener.accept().await.unwrap();
        let mut buf2 = [0u8; 2048];
        let _ = s2.read(&mut buf2).await.unwrap();
        s2.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();

        // Send a committing text delta first, so retry doesn't re-open
        let data = json!({"delta_text": "Starting output..."}).to_string();
        s2.write_all(&encode_connect_frame(0x00, data.as_bytes()))
            .await
            .unwrap();

        // Trailer with internal error
        let trailer = json!({
            "error": {
                "code": "internal",
                "message": "Model inference failed mid-stream"
            }
        })
        .to_string();
        s2.write_all(&encode_connect_frame(
            CONNECT_FLAG_END_STREAM,
            trailer.as_bytes(),
        ))
        .await
        .unwrap();
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("key"));
    let req = CompletionRequest {
        model: "devin-m1".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("stream opened");
    server.await.unwrap();

    let ev1 = stream.next().await.unwrap().expect("message start");
    assert!(matches!(ev1, StreamEvent::MessageStart { .. }));
    let ev2 = stream.next().await.unwrap().expect("text delta");
    assert_eq!(
        ev2,
        StreamEvent::TextDelta {
            text: "Starting output...".to_string()
        }
    );

    let ev3 = stream.next().await.unwrap();
    match ev3 {
        Err(ProviderError::Http { status, message }) => {
            assert_eq!(status, 500);
            assert!(message.contains("Model inference failed mid-stream"));
        }
        other => panic!("expected Http 500 error from trailer, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 6. Malformed Response Surfaces Clear Error Naming Method
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_malformed_response_in_complete_stream_surfaces_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Step 1: AssignModel succeeds
        let (mut s1, _) = listener.accept().await.unwrap();
        let mut b1 = [0u8; 1024];
        let _ = s1.read(&mut b1).await.unwrap();
        let resp1 = json!({
            "assignment": { "model_uid": "m1", "assignment_jwt": "jwt1" }
        })
        .to_string();
        s1.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                resp1.len(),
                resp1
            )
            .as_bytes(),
        )
        .await
        .unwrap();

        // Step 2: GetChatMessage sends frame with unexpected types that fail deserialization
        let (mut s2, _) = listener.accept().await.unwrap();
        let mut b2 = [0u8; 2048];
        let _ = s2.read(&mut b2).await.unwrap();
        s2.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();

        // Frame with delta_tokens as string instead of integer
        let bad_payload = json!({"delta_tokens": "invalid-non-integer"}).to_string();
        s2.write_all(&encode_connect_frame(0x00, bad_payload.as_bytes()))
            .await
            .unwrap();
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("key"));
    let req = CompletionRequest {
        model: "m1".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("stream opened");
    server.await.unwrap();

    let item = stream.next().await.expect("item arrived");
    match item {
        Err(ProviderError::Malformed(msg)) => {
            assert!(msg.contains("GetChatMessage"));
        }
        other => panic!("expected ProviderError::Malformed naming GetChatMessage, got {other:?}"),
    }
}

#[tokio::test]
async fn test_malformed_assign_model_surfaces_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await.unwrap();
        let bad_body = "not valid json";
        s.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                bad_body.len(),
                bad_body
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    });

    let provider = make_test_provider(&format!("http://127.0.0.1:{port}"), Some("key"));
    let req = CompletionRequest {
        model: "m1".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let res = provider.complete(req).await;
    server.await.unwrap();

    match res {
        Err(ProviderError::Malformed(msg)) => {
            assert!(msg.contains("AssignModel"));
        }
        Err(other) => panic!("expected ProviderError::Malformed naming AssignModel, got {other:?}"),
        Ok(_) => panic!("expected ProviderError::Malformed naming AssignModel, got Ok"),
    }
}

// ---------------------------------------------------------------------------
// 7. Idle Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_idle_timeout_surfaces_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Step 1: AssignModel
        let (mut s1, _) = listener.accept().await.unwrap();
        let mut b1 = [0u8; 1024];
        let _ = s1.read(&mut b1).await.unwrap();
        let resp1 = json!({
            "assignment": { "model_uid": "m1", "assignment_jwt": "jwt1" }
        })
        .to_string();
        s1.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                resp1.len(),
                resp1
            )
            .as_bytes(),
        )
        .await
        .unwrap();

        // Step 2: GetChatMessage opens 200 OK with chunked encoding then hangs
        let (mut s2, _) = listener.accept().await.unwrap();
        let mut b2 = [0u8; 1024];
        let _ = s2.read(&mut b2).await.unwrap();
        s2.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();

        // Sleep longer than idle timeout (idle_secs is 1 in this test)
        tokio::time::sleep(Duration::from_millis(2500)).await;
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
        stream_idle_secs: 1,
        max_retries: 0,
        ..ProviderDefaults::default()
    };

    let provider = Devin::new(
        "devin-test",
        cfg,
        defaults,
        "test-key",
        format!("http://127.0.0.1:{port}"),
        BTreeMap::new(),
    )
    .expect("valid provider");

    let req = CompletionRequest {
        model: "m1".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Hello")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("stream opened");
    let item = stream.next().await.expect("error item");
    assert!(matches!(item, Err(ProviderError::IdleTimeout(_))));
    server.abort();
}
