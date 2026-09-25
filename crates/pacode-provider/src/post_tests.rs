use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::classify_http_error;
use crate::ProviderError;

async fn read_request(socket: &mut TcpStream) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let Ok(n) = socket.read(&mut buf).await else {
            return;
        };
        if n == 0 {
            return;
        }
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        let Some(head_end) = text.find("\r\n\r\n") else {
            continue;
        };
        let content_length = text[..head_end]
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + content_length {
            return;
        }
    }
}

#[test]
fn test_status_classification_is_shared_by_every_transport() {
    let body = r#"{"error":{"message":"quota exceeded"}}"#;
    assert!(matches!(
        classify_http_error(401, body),
        ProviderError::Auth(message) if message.contains("quota exceeded")
    ));
    assert!(matches!(
        classify_http_error(403, body),
        ProviderError::Auth(_)
    ));
    assert!(matches!(
        classify_http_error(429, body),
        ProviderError::RateLimited(message) if message == "quota exceeded"
    ));
    assert!(matches!(
        classify_http_error(529, body),
        ProviderError::RateLimited(message) if message.starts_with("overloaded:")
    ));
    assert!(matches!(
        classify_http_error(500, body),
        ProviderError::Http { status: 500, message } if message == "quota exceeded"
    ));
    assert!(matches!(
        classify_http_error(400, "not json at all"),
        ProviderError::Http { status: 400, message } if message == "not json at all"
    ));
}

#[tokio::test]
async fn test_retry_after_header_is_read_from_the_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        read_request(&mut socket).await;
        let body = r#"{"error":{"message":"slow down"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });

    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/models"))
        .send()
        .await
        .expect("response");
    assert_eq!(
        crate::retry::retry_after_from_response(&response),
        Some(Duration::from_secs(3))
    );
    server.abort();
}

/// An error response that keeps streaming must not be able to hold a request
/// hostage: the body is capped and the read is on a deadline, so `complete`
/// reports the failure instead of waiting for a gateway that never stops.
#[tokio::test]
async fn test_error_body_read_is_capped_and_bounded_in_time() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        read_request(&mut socket).await;
        let head = "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\nContent-Length: 50000000\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(head.as_bytes()).await;
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..16 {
            if socket.write_all(chunk.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = socket.flush().await;
        // Never finish the declared body.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let client = reqwest::Client::new();
    let mut response = client
        .get(format!("http://127.0.0.1:{port}/chat/completions"))
        .send()
        .await
        .expect("headers arrive");
    let started = Instant::now();
    let body = crate::error_body::read_response_body_with_limits(
        &mut response,
        crate::error_body::MAX_ERROR_BODY_BYTES,
        Duration::from_millis(200),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(
        body.len() <= crate::error_body::MAX_ERROR_BODY_BYTES,
        "read {} bytes",
        body.len()
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "the read must stop at its deadline, took {elapsed:?}"
    );
    server.abort();
}
