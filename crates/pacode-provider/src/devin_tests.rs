use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{
    Effort, Message, ModelConfig, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::devin::Devin;
use crate::devin::connect::{CONNECT_FLAG_END_STREAM, ConnectFrameDecoder, encode_connect_frame};
use crate::devin::proto::{self, WireType, Writer};
use crate::devin::wire::encode_get_chat_message_request;
use crate::{CompletionRequest, Provider, ProviderError};

fn make_test_provider(base_url: &str, api_key: Option<&str>, session_token: Option<&str>) -> Devin {
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
        session_token.unwrap_or("test-session-token"),
        base_url,
        BTreeMap::new(),
    )
    .expect("valid provider")
    .with_backoff_base(Duration::from_millis(2))
}

// ---------------------------------------------------------------------------
// 1. Protobuf Codec Roundtrip (Varints, Doubles, Floats, Nested, Unknown Fields)
// ---------------------------------------------------------------------------

#[test]
fn test_proto_codec_roundtrip() {
    let mut writer = Writer::new();

    // 1. Varints at boundary values
    let boundary_varints = [
        0u64,
        1,
        127,
        128,
        255,
        256,
        16383,
        16384,
        (1 << 31) - 1,
        1 << 31,
        (1 << 63) - 1,
        u64::MAX,
    ];
    for (idx, &v) in boundary_varints.iter().enumerate() {
        writer.write_varint(10 + idx as u32, v);
    }

    // 2. Doubles (64-bit float)
    let doubles = [
        0.0f64,
        -0.0,
        1.0,
        -1.0,
        0.7,
        -12.3456789,
        std::f64::consts::PI,
        f64::MIN_POSITIVE,
        f64::MAX,
    ];
    for (idx, &d) in doubles.iter().enumerate() {
        writer.write_double(30 + idx as u32, d);
    }

    // 3. Floats (32-bit float)
    let floats = [0.0f32, 1.5, -std::f32::consts::PI, f32::MAX];
    for (idx, &f) in floats.iter().enumerate() {
        writer.write_float(50 + idx as u32, f);
    }

    // 4. Nested messages
    let mut leaf = Writer::new();
    leaf.write_string(1, "deep leaf string");
    leaf.write_varint(2, 42);

    let mut branch = Writer::new();
    branch.write_string(1, "middle branch");
    branch.write_message(2, &leaf);

    writer.write_message(70, &branch);

    // 5. Unknown fields to be skipped
    writer.write_string(80, "known start");
    writer.write_varint(99, 123456789);
    writer.write_fixed64(100, 0xdeadbeefcafebabe);
    writer.write_bytes(101, b"arbitrary length-delimited payload");
    writer.write_fixed32(102, 0x12345678);
    writer.write_string(81, "known end");

    // --- Decode and Assert ---
    let bytes = writer.into_bytes();
    let reader = proto::Reader::new(&bytes);

    let mut read_varints = Vec::new();
    let mut read_doubles = Vec::new();
    let mut read_floats = Vec::new();
    let mut read_leaf_str = String::new();
    let mut read_leaf_val = 0u64;
    let mut read_kstart = String::new();
    let mut read_kend = String::new();

    for res in reader {
        let (field_no, wire_type, val) = res.expect("valid field");
        match field_no {
            f if (10..10 + boundary_varints.len() as u32).contains(&f) => {
                assert_eq!(wire_type, WireType::Varint);
                read_varints.push(val.as_varint().unwrap());
            }
            f if (30..30 + doubles.len() as u32).contains(&f) => {
                assert_eq!(wire_type, WireType::Fixed64);
                read_doubles.push(val.as_double().unwrap());
            }
            f if (50..50 + floats.len() as u32).contains(&f) => {
                assert_eq!(wire_type, WireType::Fixed32);
                read_floats.push(val.as_float().unwrap());
            }
            70 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                let branch_reader = val.as_message().unwrap();
                for b_res in branch_reader {
                    let (bf, _, bv) = b_res.unwrap();
                    if bf == 2 {
                        let leaf_reader = bv.as_message().unwrap();
                        for l_res in leaf_reader {
                            let (lf, _, lv) = l_res.unwrap();
                            match lf {
                                1 => read_leaf_str = lv.as_str().unwrap().to_string(),
                                2 => read_leaf_val = lv.as_varint().unwrap(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            80 => read_kstart = val.as_str().unwrap().to_string(),
            81 => read_kend = val.as_str().unwrap().to_string(),
            _ => {
                // Fields 99, 100, 101, 102 are unknown and skipped automatically!
            }
        }
    }

    assert_eq!(read_varints, boundary_varints);
    for (read_d, expected_d) in read_doubles.iter().zip(doubles.iter()) {
        if expected_d.is_nan() {
            assert!(read_d.is_nan());
        } else {
            assert_eq!(read_d.to_bits(), expected_d.to_bits());
        }
    }
    for (read_f, expected_f) in read_floats.iter().zip(floats.iter()) {
        assert_eq!(read_f.to_bits(), expected_f.to_bits());
    }
    assert_eq!(read_leaf_str, "deep leaf string");
    assert_eq!(read_leaf_val, 42);
    assert_eq!(read_kstart, "known start");
    assert_eq!(read_kend, "known end");
}

// ---------------------------------------------------------------------------
// 2. Exact Byte Layout of GetChatMessage Request (Field Numbers and Order)
// ---------------------------------------------------------------------------

#[test]
fn test_exact_byte_layout_of_get_chat_message_request() {
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "You are an assistant.".to_string(),
        system_dynamic: "Current date: 2026-09-11".to_string(),
        messages: vec![Message::user("Hello Devin")],
        tools: vec![],
        effort: Some(Effort::Medium),
        max_output_tokens: Some(4096),
    };

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://server.codeium.com".to_string(),
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

    let session_token = "sess-tok-xyz-987";
    let encoded = encode_get_chat_message_request(session_token, &req, &cfg, None);

    let reader = proto::Reader::new(&encoded);
    let mut field_numbers = Vec::new();

    let mut metadata_seen = false;
    let mut system_prompt = String::new();
    let mut message_role = 0u64;
    let mut message_text = String::new();
    let mut message_id = String::new();
    let mut varint_7 = 0u64;
    let mut sampling_1 = 0u64;
    let mut sampling_max_tokens = 0u64;
    let mut pending_role = 0u64;
    let mut pending_kind = 0u64;
    let mut sampling_temperature = 0.0f64;
    let mut sampling_top_k = 0u64;
    let mut sampling_top_p = 0.0f64;
    let mut request_id = String::new();
    let mut varint_20 = 0u64;
    let mut model_uid = String::new();

    for res in reader {
        let (field_no, wire_type, val) = res.expect("valid protobuf field");
        field_numbers.push(field_no);

        match field_no {
            1 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                metadata_seen = true;
                let meta_reader = val.as_message().unwrap();
                let mut meta_subfields = Vec::new();
                for m_res in meta_reader {
                    let (mf, _, mv) = m_res.unwrap();
                    meta_subfields.push(mf);
                    match mf {
                        1 => assert_eq!(mv.as_str().unwrap(), "devin-cli"),
                        2 => assert_eq!(mv.as_str().unwrap(), "3000.10.21"),
                        3 => assert_eq!(mv.as_str().unwrap(), session_token),
                        4 => assert_eq!(mv.as_str().unwrap(), "en"),
                        5 => assert_eq!(mv.as_str().unwrap(), std::env::consts::OS),
                        7 => assert_eq!(mv.as_str().unwrap(), "3000.10.21"),
                        12 => assert_eq!(mv.as_str().unwrap(), "chisel"),
                        28 => assert_eq!(mv.as_str().unwrap(), "chisel"),
                        31 => panic!("Field 31 (fingerprint) must not be reproduced!"),
                        other => panic!("unexpected metadata field {other}"),
                    }
                }
                assert_eq!(meta_subfields, vec![1, 2, 3, 4, 5, 7, 12, 28]);
            }
            2 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                system_prompt = val.as_str().unwrap().to_string();
            }
            3 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                let msg_reader = val.as_message().unwrap();
                for m_res in msg_reader {
                    let (mf, _, mv) = m_res.unwrap();
                    match mf {
                        1 => message_id = mv.as_str().unwrap().to_string(),
                        2 => message_role = mv.as_varint().unwrap(),
                        3 => message_text = mv.as_str().unwrap().to_string(),
                        _ => {}
                    }
                }
            }
            7 => {
                assert_eq!(wire_type, WireType::Varint);
                varint_7 = val.as_varint().unwrap();
            }
            8 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                let s_reader = val.as_message().unwrap();
                for s_res in s_reader {
                    let (sf, _, sv) = s_res.unwrap();
                    match sf {
                        1 => sampling_1 = sv.as_varint().unwrap(),
                        2 => sampling_max_tokens = sv.as_varint().unwrap(),
                        5 => sampling_temperature = sv.as_double().unwrap(),
                        7 => sampling_top_k = sv.as_varint().unwrap(),
                        8 => sampling_top_p = sv.as_double().unwrap(),
                        _ => {}
                    }
                }
            }
            15 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                let p_reader = val.as_message().unwrap();
                for p_res in p_reader {
                    let (pf, _, pv) = p_res.unwrap();
                    match pf {
                        2 => pending_role = pv.as_varint().unwrap(),
                        3 => pending_kind = pv.as_varint().unwrap(),
                        _ => {}
                    }
                }
            }
            16 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                request_id = val.as_str().unwrap().to_string();
            }
            20 => {
                assert_eq!(wire_type, WireType::Varint);
                varint_20 = val.as_varint().unwrap();
            }
            21 => {
                assert_eq!(wire_type, WireType::LengthDelimited);
                model_uid = val.as_str().unwrap().to_string();
            }
            other => panic!("unexpected field number {other}"),
        }
    }

    // Assert field numbers are strictly in ascending order:
    assert_eq!(field_numbers, vec![1, 2, 3, 7, 8, 15, 16, 20, 21]);
    assert!(metadata_seen);
    assert_eq!(
        system_prompt,
        "You are an assistant.\n\nCurrent date: 2026-09-11"
    );
    assert_eq!(message_role, 1); // user
    assert_eq!(message_text, "Hello Devin");
    assert_eq!(message_id.len(), 36); // UUID v4 format
    assert_eq!(varint_7, 5);
    assert_eq!(sampling_1, 1);
    assert_eq!(sampling_max_tokens, 4096);
    // The whole sampling block goes out every time: a partial one is rejected.
    assert_eq!(sampling_temperature, 1.0);
    assert_eq!(sampling_top_k, 40);
    assert_eq!(sampling_top_p, 0.96);
    // Field 15 carries two varints, matching the official client byte for byte.
    assert_eq!(pending_role, 1);
    assert_eq!(pending_kind, 4);
    assert_eq!(request_id.len(), 36);
    assert_eq!(varint_20, 1);
    assert_eq!(model_uid, "swe-1-6-fast");
}

// ---------------------------------------------------------------------------
// 3. Auth Header Format (Basic api_key-session_token, NOT base64)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_auth_header_format() {
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
        assert!(request_str.contains("content-type: application/proto"));
        assert!(request_str.contains("connect-protocol-version: 1"));
        // Literal dash concatenation, NOT base64
        assert!(request_str.contains("authorization: Basic my-api-key-my-session-token"));

        // Empty response with 0 models
        let resp_bytes: Vec<u8> = vec![];
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            resp_bytes.len()
        );
        socket.write_all(http_resp.as_bytes()).await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("my-api-key"),
        Some("my-session-token"),
    );

    let _ = provider.list_models().await;
    server.await.unwrap();
}

// ---------------------------------------------------------------------------
// 4. Connect Frame Decode (Data, Success Trailer, Error Trailer)
// ---------------------------------------------------------------------------

#[test]
fn test_connect_frame_decode_success_and_error_trailers() {
    let mut decoder = ConnectFrameDecoder::new();

    // 1. Data frame with binary protobuf payload
    let mut pw = Writer::new();
    pw.write_string(3, "hello streaming");
    let data_payload = pw.into_bytes();
    let frame1 = encode_connect_frame(0x00, &data_payload);
    let decoded = decoder.feed(&frame1).expect("feed data frame ok");
    assert_eq!(decoded.len(), 1);
    assert!(decoded[0].is_data());
    assert_eq!(decoded[0].data().unwrap(), &data_payload);

    // 2. Trailer frame on success ({})
    let frame2 = encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}");
    let decoded2 = decoder.feed(&frame2).expect("feed success trailer ok");
    assert_eq!(decoded2.len(), 1);
    assert!(decoded2[0].is_end());
    assert!(decoded2[0].end_error().is_none());

    // 3. Trailer frame with error
    let err_trailer =
        b"{\"error\": {\"code\": \"resource_exhausted\", \"message\": \"Rate limit exceeded\"}}";
    let frame3 = encode_connect_frame(CONNECT_FLAG_END_STREAM, err_trailer);
    let decoded3 = decoder.feed(&frame3).expect("feed error trailer ok");
    assert_eq!(decoded3.len(), 1);
    assert!(decoded3[0].is_end());
    match decoded3[0].end_error().expect("error present") {
        ProviderError::RateLimited(msg) => assert!(msg.contains("Rate limit exceeded")),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. Full Streamed Turn: Text + Thinking + Usage + Stop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_streamed_turn_text_thinking_usage_stop() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req_str = String::from_utf8_lossy(&buf[..n]);

        assert!(req_str.contains("POST /exa.api_server_pb.ApiServerService/GetChatMessage"));
        assert!(req_str.contains("content-type: application/connect+proto"));
        assert!(req_str.contains("connect-protocol-version: 1"));
        assert!(req_str.contains("authorization: Basic my-api-key-my-session-token"));

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        // Frame 1: delta_thinking (field 9)
        let mut f1 = Writer::new();
        f1.write_string(1, "bot-123");
        f1.write_string(9, "Planning reasoning step...");
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        // Frame 2: delta_text (field 3)
        let mut f2 = Writer::new();
        f2.write_string(1, "bot-123");
        f2.write_string(3, "Hello, ");
        socket
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        // Frame 3: delta_text (field 3)
        let mut f3 = Writer::new();
        f3.write_string(1, "bot-123");
        f3.write_string(3, "world!");
        socket
            .write_all(&encode_connect_frame(0x00, f3.as_bytes()))
            .await
            .unwrap();

        // Frame 4: meta (field 7: input 100, output 25, cached 10, model "swe-1-6-fast")
        let mut meta = Writer::new();
        meta.write_varint(2, 100);
        meta.write_varint(3, 25);
        meta.write_varint(5, 10);
        meta.write_string(9, "swe-1-6-fast");

        let mut f4 = Writer::new();
        f4.write_string(1, "bot-123");
        f4.write_message(7, &meta);
        socket
            .write_all(&encode_connect_frame(0x00, f4.as_bytes()))
            .await
            .unwrap();

        // Frame 5: stop_reason (field 5: varint 2 = EndTurn)
        let mut f5 = Writer::new();
        f5.write_string(1, "bot-123");
        f5.write_varint(5, 2);
        socket
            .write_all(&encode_connect_frame(0x00, f5.as_bytes()))
            .await
            .unwrap();

        // Frame 6: trailer frame ({})
        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("my-api-key"),
        Some("my-session-token"),
    );
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "system".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: Some(1024),
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event ok"));
    }

    assert!(matches!(&events[0], StreamEvent::MessageStart { .. }));
    assert_eq!(
        events[1],
        StreamEvent::ReasoningDelta {
            text: "Planning reasoning step...".to_string()
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
            input_tokens: 100,
            output_tokens: 25,
            reasoning_tokens: 0,
            cache_read_tokens: 10,
            cache_write_tokens: 0,
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
// 6. Streamed Tool Call Split Across Three Frames Mid-JSON
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_streamed_tool_call_split_across_three_frames_mid_json() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        // Frame 1: split mid-JSON
        let mut f1 = Writer::new();
        f1.write_string(3, "functions.bash:0{\"comm");
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        // Frame 2: second fragment of JSON arguments
        let mut f2 = Writer::new();
        f2.write_string(3, "and\": \"ls ");
        socket
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        // Frame 3: closing fragment of JSON arguments
        let mut f3 = Writer::new();
        f3.write_string(3, "-la\"}");
        socket
            .write_all(&encode_connect_frame(0x00, f3.as_bytes()))
            .await
            .unwrap();

        // Frame 4: stop_reason
        let mut f4 = Writer::new();
        f4.write_varint(5, 2);
        socket
            .write_all(&encode_connect_frame(0x00, f4.as_bytes()))
            .await
            .unwrap();

        // Frame 5: trailer
        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("key"),
        Some("token"),
    );
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("run ls")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("event ok"));
    }

    // Assert: MessageStart -> ToolCallStart -> ToolCallArgsDelta -> MessageEnd
    assert!(matches!(&events[0], StreamEvent::MessageStart { .. }));

    // Assert exactly ONE tool call start item
    let tool_call_starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolCallStart { .. }))
        .collect();
    assert_eq!(tool_call_starts.len(), 1);

    if let StreamEvent::ToolCallStart { index, name, id } = &events[1] {
        assert_eq!(*index, 0);
        assert_eq!(name, "bash");
        assert!(!id.as_str().is_empty());
    } else {
        panic!("expected ToolCallStart at events[1], got {:?}", events[1]);
    }

    if let StreamEvent::ToolCallArgsDelta { index, delta } = &events[2] {
        assert_eq!(*index, 0);
        let parsed: serde_json::Value =
            serde_json::from_str(delta).expect("delta parses as valid JSON");
        assert_eq!(parsed["command"], "ls -la");
    } else {
        panic!(
            "expected ToolCallArgsDelta at events[2], got {:?}",
            events[2]
        );
    }

    assert_eq!(
        events[3],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );
}

// ---------------------------------------------------------------------------
// 7. Text Containing "functions" NOT Mistaken For Tool Call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_text_containing_word_functions_not_mistaken_for_tool_call() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        // Frame 1: Text with the word "functions"
        let mut f1 = Writer::new();
        f1.write_string(
            3,
            "In mathematics and computer science, functions are fundamental.",
        );
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        // Frame 2: More text with "functions" and punctuation
        let mut f2 = Writer::new();
        f2.write_string(3, " We have functions(x) = x^2 and functions. However...");
        socket
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        // Frame 3: stop_reason
        let mut f3 = Writer::new();
        f3.write_varint(5, 2);
        socket
            .write_all(&encode_connect_frame(0x00, f3.as_bytes()))
            .await
            .unwrap();

        // Frame 4: trailer
        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("key"),
        Some("token"),
    );
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("tell me about functions")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut collected_text = String::new();
    let mut saw_tool_call = false;

    while let Some(res) = stream.next().await {
        let ev = res.expect("event ok");
        match ev {
            StreamEvent::TextDelta { text } => collected_text.push_str(&text),
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {
                saw_tool_call = true;
            }
            StreamEvent::MessageEnd { stop } => {
                assert_eq!(stop, StopReason::EndTurn);
            }
            _ => {}
        }
    }

    assert!(
        !saw_tool_call,
        "ordinary text with 'functions' must NOT trigger tool call"
    );
    assert_eq!(
        collected_text,
        "In mathematics and computer science, functions are fundamental. We have functions(x) = x^2 and functions. However..."
    );
}

// ---------------------------------------------------------------------------
// 8. list_models Mapping and Merge
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
        assert!(request_str.contains("content-type: application/proto"));

        // Build protobuf response: repeated field 1 { 1 display_name, 3 float credit_cost, 18 context_window, 22 model_uid }
        let mut m1 = Writer::new();
        m1.write_string(1, "Devin Fast");
        m1.write_float(3, 1.5);
        m1.write_varint(18, 128000);
        m1.write_string(22, "swe-1-6-fast");

        let mut m2 = Writer::new();
        m2.write_string(1, "Server Overwrite Ignored");
        m2.write_string(22, "devin-custom-1");

        let mut m3 = Writer::new();
        m3.write_string(1, "Devin Deep");
        m3.write_float(3, 3.0);
        m3.write_varint(18, 256000);
        m3.write_string(22, "swe-1-6-deep");

        let mut resp = Writer::new();
        resp.write_message(1, &m1);
        resp.write_message(1, &m2);
        resp.write_message(1, &m3);

        let resp_bytes = resp.into_bytes();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            resp_bytes.len()
        );
        socket.write_all(http_resp.as_bytes()).await.unwrap();
        socket.write_all(&resp_bytes).await.unwrap();
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
        "my-token",
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

    // 2. Remote models merged in without duplicating devin-custom-1
    let ids: Vec<&str> = models.iter().map(|m| m.route.model.as_str()).collect();
    assert_eq!(ids, vec!["devin-custom-1", "swe-1-6-fast", "swe-1-6-deep"]);

    let devin_fast = models
        .iter()
        .find(|m| m.route.model == "swe-1-6-fast")
        .unwrap();
    assert_eq!(devin_fast.display_name, "Devin Fast");
    assert_eq!(devin_fast.context_window, Some(128_000));

    let devin_deep = models
        .iter()
        .find(|m| m.route.model == "swe-1-6-deep")
        .unwrap();
    assert_eq!(devin_deep.display_name, "Devin Deep");
    assert_eq!(devin_deep.context_window, Some(256_000));

    // 3. Second call hits cache (mock server has closed)
    let cached = provider.list_models().await.expect("cached models");
    assert_eq!(cached.len(), 3);
}

// ---------------------------------------------------------------------------
// 9. Error Mapping: 401 Unauthenticated, 429 RateLimited, 500 Internal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_error_mapping_unauthenticated_unary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();

        let err_body =
            "{\"code\": \"unauthenticated\", \"message\": \"Invalid session credentials\"}";
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            err_body.len(),
            err_body
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("bad-key"),
        Some("bad-token"),
    );
    let res = provider.list_models().await;
    server.await.unwrap();

    match res {
        Err(ProviderError::Auth(msg)) => assert!(msg.contains("Invalid session credentials")),
        other => panic!("expected ProviderError::Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn test_error_mapping_streaming_trailer_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = socket.read(&mut buf).await.unwrap();

        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        // Send committed text delta first so retry does not reopen
        let mut f1 = Writer::new();
        f1.write_string(3, "Starting output...");
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        // Trailer with internal error
        let trailer_err =
            b"{\"error\": {\"code\": \"internal\", \"message\": \"GPU node crashed mid-stream\"}}";
        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, trailer_err))
            .await
            .unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("key"),
        Some("token"),
    );
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
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
            assert!(message.contains("GPU node crashed mid-stream"));
        }
        other => panic!("expected Http 500 error from trailer, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 10. Idle Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_idle_timeout_surfaces_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();

        // Open 200 OK chunked stream then hang
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();

        // Hang longer than stream_idle_secs
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
        "key",
        "token",
        format!("http://127.0.0.1:{port}"),
        BTreeMap::new(),
    )
    .expect("valid provider");

    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
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

/// The server can hand a tool call over as a structured field instead of streaming it
/// inside the text, and closes such a turn with stop reason 10. Captured from live
/// traffic on 2026-09-11.
#[tokio::test]
async fn test_structured_tool_call_field_and_stop_reason_ten() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf).await.unwrap();

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        let mut thinking = Writer::new();
        thinking.write_string(1, "bot-1");
        thinking.write_string(9, "So call grep for beta");
        socket
            .write_all(&encode_connect_frame(0x00, thinking.as_bytes()))
            .await
            .unwrap();

        let mut call = Writer::new();
        call.write_string(1, "functions.grep:0");
        call.write_string(2, "grep");
        call.write_string(3, r#"{"pattern": "beta"}"#);
        let mut frame = Writer::new();
        frame.write_string(1, "bot-1");
        frame.write_message(6, &call);
        socket
            .write_all(&encode_connect_frame(0x00, frame.as_bytes()))
            .await
            .unwrap();

        let mut stop = Writer::new();
        stop.write_string(1, "bot-1");
        stop.write_varint(5, 10);
        socket
            .write_all(&encode_connect_frame(0x00, stop.as_bytes()))
            .await
            .unwrap();

        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("my-api-key"),
        Some("my-session-token"),
    );
    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "system".to_string(),
        system_dynamic: String::new(),
        messages: vec![Message::user("find beta")],
        tools: vec![],
        effort: None,
        max_output_tokens: Some(1024),
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event ok"));
    }

    let names: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCallStart { name, index, .. } => Some((name.clone(), *index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![("grep".to_string(), 0)],
        "structured call surfaced"
    );

    let args: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCallArgsDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(args, vec![r#"{"pattern": "beta"}"#.to_string()]);

    let stop = events.iter().rev().find_map(|e| match e {
        StreamEvent::MessageEnd { stop } => Some(stop.clone()),
        _ => None,
    });
    assert_eq!(
        stop,
        Some(StopReason::ToolUse),
        "stop reason 10 is tool use"
    );
}

// ---------------------------------------------------------------------------
// 11. Adaptive Model Routing (AssignModel -> GetChatMessage with JWT and assigned uid)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_adaptive_route_issues_exactly_one_assign_model_and_carries_jwt_and_assigned_uid() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        // Request 1: AssignModel
        let (mut socket1, _) = listener.accept().await.unwrap();
        let mut buf1 = [0u8; 4096];
        let n1 = socket1.read(&mut buf1).await.unwrap();
        let req_str1 = String::from_utf8_lossy(&buf1[..n1]);

        assert!(
            req_str1.contains("POST /exa.api_server_pb.ApiServerService/AssignModel"),
            "First call must be AssignModel"
        );
        assert!(req_str1.contains("content-type: application/proto"));

        let body_offset1 = req_str1.find("\r\n\r\n").unwrap() + 4;
        let body1 = &buf1[body_offset1..n1];
        let reader1 = proto::Reader::new(body1);

        let mut assign_model_uid = String::new();
        let mut assign_conv_id = String::new();
        let mut assign_user_text = String::new();
        let mut assign_user_role = 0u64;

        for res in reader1 {
            let (f, _, v) = res.unwrap();
            match f {
                2 => assign_model_uid = v.as_str().unwrap().to_string(),
                3 => assign_conv_id = v.as_str().unwrap().to_string(),
                5 => {
                    let msg_reader = v.as_message().unwrap();
                    for m_res in msg_reader {
                        let (mf, _, mv) = m_res.unwrap();
                        match mf {
                            2 => assign_user_role = mv.as_varint().unwrap(),
                            3 => assign_user_text = mv.as_str().unwrap().to_string(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        assert_eq!(assign_model_uid, "adaptive");
        assert_eq!(assign_conv_id.len(), 36, "conversation_id must be a UUID");
        assert_eq!(assign_user_role, 1);
        assert_eq!(assign_user_text, "solve problem");

        // 1 assignment { 1 assignment_jwt, 2 assigned_model_uid, 3 repeated harness_uid }
        let mut assignment_w = Writer::new();
        assignment_w.write_string(1, "test.assignment.jwt.token");
        assignment_w.write_string(2, "gpt-5-6-sol-low");
        assignment_w.write_string(3, "harness-default");

        let mut assign_resp = Writer::new();
        assign_resp.write_message(1, &assignment_w);
        let resp_payload = assign_resp.into_bytes();

        let http_resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            resp_payload.len()
        );
        socket1.write_all(http_resp.as_bytes()).await.unwrap();
        socket1.write_all(&resp_payload).await.unwrap();
        socket1.shutdown().await.unwrap();

        // Request 2: GetChatMessage
        let (mut socket2, _) = listener.accept().await.unwrap();
        let mut buf2 = [0u8; 8192];
        let mut total_read = 0;
        let mut header_len = 0;
        let mut content_len = 0;
        loop {
            let n = socket2.read(&mut buf2[total_read..]).await.unwrap();
            if n == 0 {
                break;
            }
            total_read += n;
            let s = String::from_utf8_lossy(&buf2[..total_read]);
            if let Some(pos) = s.find("\r\n\r\n") {
                header_len = pos + 4;
                for line in s[..pos].lines() {
                    if let Some(val) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_len = val.trim().parse::<usize>().unwrap_or(0);
                    }
                }
                if total_read >= header_len + content_len {
                    break;
                }
            }
        }

        let req_str2 = String::from_utf8_lossy(&buf2[..header_len]);
        assert!(
            req_str2.contains("POST /exa.api_server_pb.ApiServerService/GetChatMessage"),
            "Second call must be GetChatMessage"
        );
        assert!(req_str2.contains("content-type: application/connect+proto"));

        let body2 = &buf2[header_len..header_len + content_len];
        assert!(body2.len() >= 5);
        let proto_body2 = &body2[5..];

        let reader2 = proto::Reader::new(proto_body2);
        let mut chat_model_uid = String::new();
        let mut chat_assignment_jwt = String::new();

        for res in reader2 {
            let (f, _, v) = res.unwrap();
            match f {
                21 => chat_model_uid = v.as_str().unwrap().to_string(),
                26 => chat_assignment_jwt = v.as_str().unwrap().to_string(),
                _ => {}
            }
        }

        assert_eq!(
            chat_model_uid, "gpt-5-6-sol-low",
            "field 21 must be the assigned model uid"
        );
        assert_eq!(
            chat_assignment_jwt, "test.assignment.jwt.token",
            "field 26 must carry the assignment jwt"
        );

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket2.write_all(header.as_bytes()).await.unwrap();

        let mut f1 = Writer::new();
        f1.write_string(1, "bot-adaptive");
        f1.write_string(3, "Adaptive solution");
        socket2
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        let mut f2 = Writer::new();
        f2.write_string(1, "bot-adaptive");
        f2.write_varint(5, 2);
        socket2
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        socket2
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket2.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("my-api-key"),
        Some("my-session-token"),
    );

    let req = CompletionRequest {
        model: "adaptive".to_string(),
        system_static: "system".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("solve problem")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut text = String::new();
    while let Some(res) = stream.next().await {
        let ev = res.expect("event ok");
        if let StreamEvent::TextDelta { text: d } = ev {
            text.push_str(&d);
        }
    }
    assert_eq!(text, "Adaptive solution");
}

// ---------------------------------------------------------------------------
// 12. Non-Adaptive Route Issues No AssignModel Call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_non_adaptive_route_issues_no_assign_model() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let mut total_read = 0;
        let mut header_len = 0;
        let mut content_len = 0;
        loop {
            let n = socket.read(&mut buf[total_read..]).await.unwrap();
            if n == 0 {
                break;
            }
            total_read += n;
            let s = String::from_utf8_lossy(&buf[..total_read]);
            if let Some(pos) = s.find("\r\n\r\n") {
                header_len = pos + 4;
                for line in s[..pos].lines() {
                    if let Some(val) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_len = val.trim().parse::<usize>().unwrap_or(0);
                    }
                }
                if total_read >= header_len + content_len {
                    break;
                }
            }
        }

        let req_str = String::from_utf8_lossy(&buf[..header_len]);
        assert!(
            req_str.contains("POST /exa.api_server_pb.ApiServerService/GetChatMessage"),
            "Must directly call GetChatMessage without AssignModel"
        );
        assert!(!req_str.contains("AssignModel"));

        let body = &buf[header_len..header_len + content_len];
        let proto_body = &body[5..];
        let reader = proto::Reader::new(proto_body);

        let mut model_uid = String::new();
        let mut has_field_26 = false;

        for res in reader {
            let (f, _, v) = res.unwrap();
            match f {
                21 => model_uid = v.as_str().unwrap().to_string(),
                26 => has_field_26 = true,
                _ => {}
            }
        }

        assert_eq!(model_uid, "swe-1-6-fast");
        assert!(
            !has_field_26,
            "Field 26 must not be written when no assignment exists"
        );

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        let mut f1 = Writer::new();
        f1.write_string(1, "bot-direct");
        f1.write_string(3, "Direct output");
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        let mut f2 = Writer::new();
        f2.write_string(1, "bot-direct");
        f2.write_varint(5, 2);
        socket
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("my-api-key"),
        Some("my-session-token"),
    );

    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut text = String::new();
    while let Some(res) = stream.next().await {
        if let StreamEvent::TextDelta { text: d } = res.expect("event ok") {
            text.push_str(&d);
        }
    }
    assert_eq!(text, "Direct output");
}

// ---------------------------------------------------------------------------
// 13. Response Carrying Fields 10 and 21 Produces ReasoningSignature Event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_response_carrying_fields_10_and_21_produces_reasoning_signature_event() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let header = "HTTP/1.1 200 OK\r\ncontent-type: application/connect+proto\r\nconnection: close\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        // Frame 1 carries fields 9 (thinking), 10 (signature), 15 (msg id), 21 (signature kind)
        let mut f1 = Writer::new();
        f1.write_string(1, "bot-sig-1");
        f1.write_string(9, "Thinking deeply about the problem...");
        f1.write_string(10, "sealed.v1.e30.signature_test");
        f1.write_string(15, "msg_devin_live_capture_123");
        f1.write_string(21, "sealed");
        socket
            .write_all(&encode_connect_frame(0x00, f1.as_bytes()))
            .await
            .unwrap();

        // Frame 2: text delta
        let mut f2 = Writer::new();
        f2.write_string(1, "bot-sig-1");
        f2.write_string(3, "Here is the result");
        socket
            .write_all(&encode_connect_frame(0x00, f2.as_bytes()))
            .await
            .unwrap();

        // Frame 3: stop
        let mut f3 = Writer::new();
        f3.write_string(1, "bot-sig-1");
        f3.write_varint(5, 2);
        socket
            .write_all(&encode_connect_frame(0x00, f3.as_bytes()))
            .await
            .unwrap();

        socket
            .write_all(&encode_connect_frame(CONNECT_FLAG_END_STREAM, b"{}"))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let provider = make_test_provider(
        &format!("http://127.0.0.1:{port}"),
        Some("key"),
        Some("token"),
    );

    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("do something")],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await.expect("complete succeeds");
    server.await.unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("event ok"));
    }

    let sig_event = events
        .iter()
        .find(|e| matches!(e, StreamEvent::ReasoningSignature { .. }));

    assert_eq!(
        sig_event,
        Some(&StreamEvent::ReasoningSignature {
            signature: "sealed.v1.e30.signature_test".to_string(),
            kind: Some("sealed".to_string()),
        })
    );
}

// ---------------------------------------------------------------------------
// 14. Assistant Message with Signature Re-Encoded as Fields 11, 12, 18
// ---------------------------------------------------------------------------

#[test]
fn test_assistant_message_with_signature_re_encoded_as_11_12_18() {
    let assistant_msg = Message::new(
        pacode_types::Role::Assistant,
        vec![
            pacode_types::ContentBlock::Reasoning {
                text: "My captured thoughts".to_string(),
                signature: Some("sealed.v1.dGVzdF9zaWduYXR1cmU=".to_string()),
            },
            pacode_types::ContentBlock::Text {
                text: "My final answer".to_string(),
            },
        ],
    );

    let req = CompletionRequest {
        model: "swe-1-6-fast".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![Message::user("Question"), assistant_msg],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://server.codeium.com".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![],
        catalog: false,
        context_window: None,
        reasoning: Some(true),
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
    };

    let encoded = encode_get_chat_message_request("session-token", &req, &cfg, None);
    let reader = proto::Reader::new(&encoded);

    let mut found_assistant = false;
    let mut assistant_role = 0u64;
    let mut assistant_text = String::new();
    let mut thinking_text = String::new();
    let mut thinking_sig = String::new();
    let mut thinking_kind = String::new();

    for res in reader {
        let (f, _, v) = res.unwrap();
        if f == 3 {
            let msg_reader = v.as_message().unwrap();
            let mut role = 0u64;
            let mut text = String::new();
            let mut thinking = String::new();
            let mut sig = String::new();
            let mut kind = String::new();

            for m_res in msg_reader {
                let (mf, _, mv) = m_res.unwrap();
                match mf {
                    2 => role = mv.as_varint().unwrap(),
                    3 => text = mv.as_str().unwrap().to_string(),
                    11 => thinking = mv.as_str().unwrap().to_string(),
                    12 => sig = mv.as_str().unwrap().to_string(),
                    18 => kind = mv.as_str().unwrap().to_string(),
                    _ => {}
                }
            }

            if role == 2 {
                found_assistant = true;
                assistant_role = role;
                assistant_text = text;
                thinking_text = thinking;
                thinking_sig = sig;
                thinking_kind = kind;
            }
        }
    }

    assert!(found_assistant, "assistant message must be found");
    assert_eq!(assistant_role, 2);
    assert_eq!(assistant_text, "My final answer");
    assert_eq!(
        thinking_text, "My captured thoughts",
        "field 11 is thinking text"
    );
    assert_eq!(
        thinking_sig, "sealed.v1.dGVzdF9zaWduYXR1cmU=",
        "field 12 is signature"
    );
    assert_eq!(thinking_kind, "sealed", "field 18 is signature kind");
}

/// Field 16 is the conversation id, not a fresh per-request uuid: the assignment token
/// AssignModel hands out is bound to it, and sending a different one made the server
/// answer invalid_argument. Caught against the live service on 2026-09-11.
#[test]
fn test_conversation_id_is_stable_across_turns_of_one_conversation() {
    use crate::devin::wire::conversation_id;

    let first = CompletionRequest {
        model: "adaptive".to_string(),
        system_static: "system".to_string(),
        system_dynamic: String::new(),
        messages: vec![Message::user("find gamma")],
        tools: vec![],
        effort: None,
        max_output_tokens: Some(1024),
    };
    let mut later = first.clone();
    later.messages.push(Message::assistant_text("looking"));
    later.messages.push(Message::user("and now delta"));
    later.system_dynamic = "changed between turns".to_string();

    assert_eq!(
        conversation_id(&first),
        conversation_id(&later),
        "appending turns must not change the conversation id"
    );

    let other = CompletionRequest {
        messages: vec![Message::user("a different conversation")],
        ..first.clone()
    };
    assert_ne!(
        conversation_id(&first),
        conversation_id(&other),
        "separate conversations must not share an id"
    );

    let id = conversation_id(&first);
    assert_eq!(id.len(), 36, "uuid shaped: {id}");
    assert_eq!(id.as_bytes()[14], b'4', "version nibble: {id}");
}
