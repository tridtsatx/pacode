use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use futures::StreamExt;
use pacode_types::{ProviderConfig, ProviderDefaults, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::governor::{ProviderGovernor, governor_for};
use crate::retry::{
    OpenedStream, RetryAfterSignal, RetryBudget, StreamTransport, backoff_delay, parse_retry_after,
    retrying_transport_stream,
};
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

async fn write_http(socket: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &str) {
    let mut response = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    response.push_str("Connection: close\r\n\r\n");
    response.push_str(body);
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn provider_for(port: u16, max_retries: u32) -> OpenAiCompat {
    provider_with_idle_secs(
        port,
        max_retries,
        ProviderDefaults::default().stream_idle_secs,
    )
}

fn provider_with_idle_secs(port: u16, max_retries: u32, stream_idle_secs: u64) -> OpenAiCompat {
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
        ..Default::default()
    };
    let defaults = ProviderDefaults {
        max_retries,
        stream_idle_secs,
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
            | StreamEvent::ReasoningSignature { .. }
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
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept");
            server_connections.fetch_add(1, Ordering::SeqCst);
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
        connections.load(Ordering::SeqCst),
        1,
        "the request must not be reopened once content was emitted"
    );
    server.abort();
}

/// A server that accepts the connection and reads the request but never writes
/// response headers must not hang `complete` forever: the header wait is bounded
/// by the same idle budget as the SSE body phase.
#[tokio::test]
async fn test_response_headers_timeout_surfaces_idle_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        read_request(&mut socket).await;
        // Never answer; keep the socket open so send() cannot resolve or fail.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let provider = provider_with_idle_secs(port, 0, 1);
    match provider.complete(request()).await {
        Err(ProviderError::IdleTimeout(secs)) => assert_eq!(secs, 1),
        Err(other) => panic!("expected IdleTimeout, got {other:?}"),
        Ok(_) => panic!("header wait must time out instead of opening a stream"),
    }
    server.abort();
}

#[tokio::test]
async fn test_retry_budget_is_shared_by_opener_and_mid_stream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let mut served = 0usize;
        while served < 3 {
            let Ok(accepted) =
                tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
            else {
                break;
            };
            let (mut socket, _) = accepted.expect("accept");
            read_request(&mut socket).await;
            match served {
                0 => {
                    write_http(
                        &mut socket,
                        "500 Internal Server Error",
                        &[("Content-Type", "application/json")],
                        r#"{"error":{"message":"opener failed"}}"#,
                    )
                    .await;
                }
                1 => write_sse(&mut socket, MID_STREAM_429).await,
                _ => write_sse(&mut socket, TEXT_THEN_DONE).await,
            }
            served += 1;
        }
        served
    });

    let provider = provider_for(port, 1);
    let mut stream = provider.complete(request()).await.expect("stream opens");
    let error = loop {
        match stream.next().await {
            Some(Err(error)) => break error,
            Some(Ok(_)) => {}
            None => panic!("expected the exhausted retry budget to surface an error"),
        }
    };
    assert!(matches!(error, ProviderError::RateLimited(_)));
    assert_eq!(server.await.expect("server task"), 2);
}

const MID_STREAM_500: &str =
    "data: {\"error\":{\"message\":\"upstream exploded\",\"code\":500}}\n\n";

/// Every attempt fails to connect. The budget is the only thing that stops the
/// loop, so the connection count is exactly `max_retries + 1` — a transport that
/// nested one budget per layer would multiply them.
#[tokio::test]
async fn test_open_failures_do_not_multiply_retries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    let server = tokio::spawn(async move {
        loop {
            let Ok(accepted) =
                tokio::time::timeout(Duration::from_secs(5), listener.accept()).await
            else {
                break;
            };
            let (mut socket, _) = accepted.expect("accept");
            server_connections.fetch_add(1, Ordering::SeqCst);
            read_request(&mut socket).await;
            write_http(
                &mut socket,
                "500 Internal Server Error",
                &[("Content-Type", "application/json")],
                r#"{"error":{"message":"upstream exploded"}}"#,
            )
            .await;
        }
    });

    let provider = provider_for(port, 2);
    let error = match provider.complete(request()).await {
        Err(error) => error,
        Ok(_) => panic!("the budget runs out before a stream opens"),
    };
    assert!(
        matches!(error, ProviderError::Http { status: 500, .. }),
        "{error:?}"
    );
    assert_eq!(
        connections.load(Ordering::SeqCst),
        3,
        "one attempt plus max_retries, not one budget per layer"
    );
    server.abort();
}

/// Same bound, but the failure arrives after the response headers: the wrapper
/// reopens, and those reopens draw on what the opener left of the budget.
#[tokio::test]
async fn test_mid_stream_failures_do_not_multiply_retries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    let server = tokio::spawn(async move {
        loop {
            let Ok(accepted) =
                tokio::time::timeout(Duration::from_secs(5), listener.accept()).await
            else {
                break;
            };
            let (mut socket, _) = accepted.expect("accept");
            server_connections.fetch_add(1, Ordering::SeqCst);
            read_request(&mut socket).await;
            write_sse(&mut socket, MID_STREAM_500).await;
        }
    });

    let provider = provider_for(port, 2);
    let mut stream = provider.complete(request()).await.expect("stream opens");
    let error = loop {
        match stream.next().await {
            Some(Err(error)) => break error,
            Some(Ok(_)) => {}
            None => panic!("expected the exhausted retry budget to surface an error"),
        }
    };
    assert!(
        matches!(error, ProviderError::Http { status: 500, .. }),
        "{error:?}"
    );
    assert_eq!(
        connections.load(Ordering::SeqCst),
        3,
        "reopens share the opener's budget"
    );
    server.abort();
}

/// A `Retry-After` seen inside the stream parks the whole account, so the next
/// request for it waits instead of walking into the same rate limit.
#[tokio::test]
async fn test_stream_retry_after_parks_the_account() {
    let opens = Arc::new(AtomicUsize::new(0));
    let transport = ProbeTransport {
        governor: governor_for("https://probe.test/v1/chat", None, &[]),
        opens: Arc::clone(&opens),
    };
    let governor = transport.governor();
    let retry_after = RetryAfterSignal::default();
    retry_after.set(Duration::from_secs(2));
    let opened = OpenedStream {
        stream: futures::stream::once(async {
            Err(ProviderError::RateLimited("slow down".to_string()))
        })
        .boxed(),
        retry_after,
    };

    let errors = drain(retrying_transport_stream(
        transport,
        governor.clone(),
        opened,
        RetryBudget::new(0),
    ))
    .await;

    assert!(matches!(errors.as_slice(), [ProviderError::RateLimited(_)]));
    assert_eq!(opens.load(Ordering::SeqCst), 0, "no budget, no reopen");
    let remaining = governor.cooldown_remaining();
    assert!(
        remaining > Duration::from_secs(1),
        "the signal from the stream must park the account, got {remaining:?}"
    );
    governor.start_cooldown(Duration::ZERO);
}

#[derive(Clone)]
struct ProbeTransport {
    governor: Arc<ProviderGovernor>,
    opens: Arc<AtomicUsize>,
}

impl StreamTransport for ProbeTransport {
    fn governor(&self) -> Arc<ProviderGovernor> {
        Arc::clone(&self.governor)
    }

    fn backoff_base(&self) -> Duration {
        Duration::from_millis(1)
    }

    async fn open_once(
        &self,
        _governor: &Arc<ProviderGovernor>,
        _budget: &mut RetryBudget,
    ) -> Result<OpenedStream, ProviderError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Transport("probe".to_string()))
    }
}

async fn drain(mut stream: crate::EventStream) -> Vec<ProviderError> {
    let mut errors = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => panic!("the probe never succeeds"),
            Err(error) => errors.push(error),
        }
    }
    errors
}

#[test]
fn test_retry_after_and_jitter_are_bounded() {
    assert_eq!(
        parse_retry_after("7", UNIX_EPOCH),
        Some(Duration::from_secs(7))
    );
    let epoch = UNIX_EPOCH + Duration::from_secs(784_111_777);
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", epoch),
        Some(Duration::ZERO)
    );
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:50:37 GMT", epoch),
        Some(Duration::from_secs(60))
    );

    for _ in 0..100 {
        let delay = backoff_delay(0, Duration::from_secs(1));
        assert!(delay >= Duration::from_millis(800));
        assert!(delay <= Duration::from_millis(1200));
    }
}

#[test]
fn test_retry_budget_cannot_be_over_reserved() {
    let mut budget = RetryBudget::new(1);
    assert!(budget.reserve());
    assert!(!budget.reserve());
    assert_eq!(budget.used, 1);
}
