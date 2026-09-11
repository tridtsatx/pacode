use std::collections::BTreeMap;

use futures::StreamExt;
use pacode_provider::{CompletionRequest, OpenAiCompat, Provider};
use pacode_types::{CallId, ProviderConfig, ProviderDefaults, StopReason, StreamEvent, Usage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut temp = [0u8; 1024];
    let mut header_end = None;
    let mut content_length = 0;

    loop {
        let n = stream.read(&mut temp).await.expect("read tcp");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);

        if header_end.is_none()
            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            header_end = Some(pos + 4);
            let headers_str = String::from_utf8_lossy(&buf[..pos]);
            for line in headers_str.lines() {
                let lower = line.to_ascii_lowercase();
                if let Some(val) = lower.strip_prefix("content-length:") {
                    content_length = val.trim().parse::<usize>().unwrap_or(0);
                }
            }
        }

        if let Some(hend) = header_end
            && buf.len() >= hend + content_length
        {
            break;
        }
    }

    let hend = header_end.unwrap_or(buf.len());
    let headers = String::from_utf8_lossy(&buf[..hend]).to_string();
    let body = buf[hend..].to_vec();
    (headers, body)
}

#[tokio::test]
async fn test_e2e_retry_and_sse_stream() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp listener");
    let port = listener.local_addr().expect("local addr").port();

    let server_task = tokio::spawn(async move {
        // First connection: respond with 503
        let (mut sock1, _) = listener.accept().await.expect("accept conn 1");
        let (headers1, _body1) = read_http_request(&mut sock1).await;
        assert!(
            headers1.contains("authorization: Bearer test-secret-key")
                || headers1.contains("Authorization: Bearer test-secret-key"),
            "request should contain Authorization header: {headers1}"
        );
        assert!(
            headers1.contains("content-type: application/json")
                || headers1.contains("Content-Type: application/json"),
            "request should contain Content-Type header: {headers1}"
        );

        let response_503 = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 19\r\nConnection: close\r\n\r\nService Unavailable";
        sock1
            .write_all(response_503.as_bytes())
            .await
            .expect("write 503");
        sock1.flush().await.expect("flush 503");
        let _ = sock1.shutdown().await;
        drop(sock1);

        // Second connection (retry): respond with 200 OK SSE stream
        let (mut sock2, _) = listener.accept().await.expect("accept conn 2");
        let (headers2, _body2) = read_http_request(&mut sock2).await;
        assert!(
            headers2.contains("authorization: Bearer test-secret-key")
                || headers2.contains("Authorization: Bearer test-secret-key")
        );

        let sse_body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello from \"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"server!\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_e2e\",\"function\":{\"name\":\"calc\",\"arguments\":\"{\\\"val\\\":42}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"completion_tokens_details\":{\"reasoning_tokens\":0},\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
            "data: [DONE]\n\n"
        );
        let response_200 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            sse_body.len(),
            sse_body
        );
        sock2
            .write_all(response_200.as_bytes())
            .await
            .expect("write 200");
        sock2.flush().await.expect("flush 200");
        let _ = sock2.shutdown().await;
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
        max_retries: 3,
        stream_idle_secs: 15,
        ..ProviderDefaults::default()
    };

    let provider = OpenAiCompat::new(
        "test-e2e",
        cfg,
        defaults,
        Some("test-secret-key".to_string()),
        BTreeMap::new(),
    )
    .expect("provider created");

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        system_static: "System test".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider
        .complete(req)
        .await
        .expect("complete succeeds after retry");

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event ok"));
    }

    server_task.await.expect("server completed");

    // Verify resulting event sequence
    assert_eq!(events.len(), 7);

    // 1. MessageStart emitted before first delta
    match &events[0] {
        StreamEvent::MessageStart { model } => {
            assert_eq!(model.as_deref(), Some("gpt-4o"));
        }
        other => panic!("expected MessageStart, got {other:?}"),
    }

    // 2. TextDelta 1
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            text: "Hello from ".to_string()
        }
    );

    // 3. TextDelta 2
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            text: "server!".to_string()
        }
    );

    // 4. ToolCallStart
    assert_eq!(
        events[3],
        StreamEvent::ToolCallStart {
            index: 0,
            id: CallId::new("call_e2e"),
            name: "calc".to_string(),
        }
    );

    // 5. ToolCallArgsDelta
    assert_eq!(
        events[4],
        StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: "{\"val\":42}".to_string(),
        }
    );

    // 6. MessageEnd
    assert_eq!(
        events[5],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );

    // 7. Usage
    assert_eq!(
        events[6],
        StreamEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: 5,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        })
    );
}
