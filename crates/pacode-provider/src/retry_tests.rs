use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{ProviderConfig, ProviderDefaults, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{CompletionRequest, OpenAiCompat, Provider, ProviderError};

/// Read a whole HTTP request (headers + `Content-Length` body) so the client never sees
/// its write fail while we are already answering.
async fn read_request(socket: &mut TcpStream) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buf[..n]);

        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        let Some(head_end) = text.find("\r\n\r\n") else {
            continue;
        };
        let content_length = text[..head_end]
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + content_length {
            return;
        }
    }
}

async fn write_sse(socket: &mut TcpStream, body: &str) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(body.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn provider_for(port: u16, max_retries: u32) -> OpenAiCompat {
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
        max_retries,
        ..ProviderDefaults::default()
    };
    OpenAiCompat::new("test-prov", cfg, defaults, None, BTreeMap::new())
        .expect("provider builds")
        // Real retries, no real waiting.
        .with_backoff_base(Duration::from_millis(1))
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "test-model".to_string(),
        system_static: String::new(),
        system_dynamic: String::new(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    }
}

const MID_STREAM_429: &str = concat!(
    "data: {\"error\":{\"message\":\"Provider returned error\",\"code\":429,",
    "\"metadata\":{\"raw\":\"temporarily rate-limited upstream\"}}}\n\n"
);

const TEXT_THEN_DONE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

#[tokio::test]
async fn test_mid_stream_rate_limit_reopens_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let server = tokio::spawn(async move {
        let mut served = 0usize;
        while served < 2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            read_request(&mut socket).await;
            if served == 0 {
                write_sse(&mut socket, MID_STREAM_429).await;
            } else {
                write_sse(&mut socket, TEXT_THEN_DONE).await;
            }
            served += 1;
        }
        served
    });

    let provider = provider_for(port, 3);
    let mut stream = provider.complete(request()).await.expect("stream opens");

    let mut texts = Vec::new();
    while let Some(item) = stream.next().await {
        match item.expect("no error should reach the caller") {
            StreamEvent::TextDelta { text } => texts.push(text),
            StreamEvent::MessageStart { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallArgsDelta { .. }
            | StreamEvent::Usage(_)
            | StreamEvent::MessageEnd { .. } => {}
        }
    }

    assert_eq!(texts, vec!["hello".to_string()]);
    assert_eq!(
        server.await.expect("server task"),
        2,
        "request was reopened"
    );
}

#[tokio::test]
async fn test_error_after_content_is_not_retried() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let body = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"partial\"}}}}]}}\n\n{MID_STREAM_429}"
    );

    // A second connection would mean an unwanted retry; the counter proves it never happens.
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = std::sync::Arc::clone(&connections);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept");
            server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            read_request(&mut socket).await;
            write_sse(&mut socket, &body).await;
        }
    });

    let provider = provider_for(port, 3);
    let mut stream = provider.complete(request()).await.expect("stream opens");

    let mut texts = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::TextDelta { text }) => texts.push(text),
            Ok(_) => {}
            Err(err) => {
                error = Some(err);
                break;
            }
        }
    }

    assert_eq!(texts, vec!["partial".to_string()]);
    match error {
        Some(ProviderError::RateLimited(msg)) => {
            assert!(
                msg.contains("temporarily rate-limited upstream"),
                "got {msg}"
            );
        }
        Some(other) => panic!("expected RateLimited, got {other:?}"),
        None => panic!("expected the stream error to reach the caller"),
    }

    assert_eq!(
        connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the request must not be reopened once content was emitted"
    );
    server.abort();
}
