use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use pacode_types::{ProviderConfig, ProviderDefaults, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{GateMap, GatedStream, ProviderGate};
use crate::{CompletionRequest, EventStream, OpenAiCompat, Provider};

const SHORT: Duration = Duration::from_millis(40);

#[tokio::test]
async fn never_more_than_limit_permits() {
    let gate = Arc::new(ProviderGate::new(2));
    let a = gate.acquire().await;
    let _b = gate.acquire().await;
    assert_eq!(gate.available(), Some(0));

    // The third caller queues until a permit is released.
    let waiter = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move { gate.acquire().await })
    };
    tokio::time::sleep(SHORT).await;
    assert!(!waiter.is_finished());
    assert_eq!(gate.queued(), 1);

    drop(a);
    let _c = waiter.await.expect("waiter joins");
    assert_eq!(gate.queued(), 0);
    assert_eq!(gate.available(), Some(0));
}

#[tokio::test]
async fn zero_means_unlimited() {
    let gate = ProviderGate::new(0);
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(gate.acquire().await);
    }
    assert_eq!(gate.available(), None);
    assert_eq!(gate.queued(), 0);
}

#[tokio::test]
async fn cancelled_acquire_leaks_nothing() {
    let gate = Arc::new(ProviderGate::new(1));
    let held = gate.acquire().await;

    let waiter = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move { gate.acquire().await })
    };
    tokio::time::sleep(SHORT).await;
    assert_eq!(gate.queued(), 1);
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(gate.queued(), 0, "queue slot freed on cancellation");

    drop(held);
    assert_eq!(gate.available(), Some(1), "no permit leaked");
    let _again = tokio::time::timeout(Duration::from_secs(1), gate.acquire())
        .await
        .expect("permit is free again");
}

#[tokio::test]
async fn cancelled_cooldown_wait_is_prompt() {
    let gate = Arc::new(ProviderGate::new(1));
    gate.start_cooldown(Duration::from_secs(30));
    let res = tokio::time::timeout(SHORT, gate.acquire()).await;
    assert!(res.is_err(), "still cooling down");
    assert_eq!(gate.queued(), 0);
    assert_eq!(gate.available(), Some(1));
}

#[tokio::test]
async fn cooldown_delays_every_waiter() {
    let gate = Arc::new(ProviderGate::new(4));
    gate.start_cooldown(Duration::from_millis(150));
    let started = tokio::time::Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let gate = Arc::clone(&gate);
        tasks.push(tokio::spawn(async move {
            let _p = gate.acquire().await;
            started.elapsed()
        }));
    }
    for t in tasks {
        let waited = t.await.expect("task joins");
        assert!(waited >= Duration::from_millis(140), "waited {waited:?}");
    }
    assert!(gate.cooldown_remaining().is_none());
}

#[tokio::test]
async fn cooldown_never_shortens_and_is_clamped() {
    let gate = ProviderGate::new(1);
    gate.start_cooldown(Duration::from_secs(10));
    gate.start_cooldown(Duration::from_millis(1));
    let left = gate.cooldown_remaining().expect("still cooling");
    assert!(left > Duration::from_secs(5));

    let gate = ProviderGate::new(1);
    gate.start_cooldown(Duration::from_secs(86_400));
    let left = gate.cooldown_remaining().expect("cooling");
    assert!(left <= super::MAX_COOLDOWN);
}

#[tokio::test]
async fn note_response_only_for_429_503_with_hint() {
    use reqwest::header::{HeaderMap, HeaderValue};
    let gate = ProviderGate::new(1);
    let mut h = HeaderMap::new();
    assert_eq!(gate.note_response(429, &h), None);
    h.insert("retry-after", HeaderValue::from_static("5"));
    assert_eq!(gate.note_response(500, &h), None);
    assert!(gate.cooldown_remaining().is_none());
    assert_eq!(gate.note_response(429, &h), Some(Duration::from_secs(5)));
    assert!(gate.cooldown_remaining().is_some());
}

fn event_stream(items: Vec<Result<StreamEvent, crate::ProviderError>>) -> EventStream {
    Box::pin(futures::stream::iter(items))
}

#[tokio::test]
async fn dropped_stream_releases_permit() {
    let gate = Arc::new(ProviderGate::new(1));
    let permit = gate.acquire().await;
    let stream = GatedStream::wrap(
        Box::pin(futures::stream::pending::<
            Result<StreamEvent, crate::ProviderError>,
        >()),
        permit,
    );
    assert_eq!(gate.available(), Some(0));
    drop(stream);
    assert_eq!(gate.available(), Some(1));
}

#[tokio::test]
async fn finished_or_failed_stream_releases_permit() {
    let gate = Arc::new(ProviderGate::new(1));

    let permit = gate.acquire().await;
    let mut ok = GatedStream::wrap(event_stream(vec![]), permit);
    assert_eq!(gate.available(), Some(0));
    assert!(ok.next().await.is_none());
    assert_eq!(
        gate.available(),
        Some(1),
        "released at end, stream still alive"
    );

    let permit = gate.acquire().await;
    let mut failing = GatedStream::wrap(
        event_stream(vec![Err(crate::ProviderError::Transport("x".into()))]),
        permit,
    );
    assert!(matches!(failing.next().await, Some(Err(_))));
    assert_eq!(gate.available(), Some(1), "released on error");
}

#[test]
fn gate_map_shares_and_replaces_on_limit_change() {
    let map = GateMap::default();
    let a = map.gate_for("p", 4);
    let b = map.gate_for("p", 4);
    assert!(Arc::ptr_eq(&a, &b));
    let other = map.gate_for("q", 4);
    assert!(!Arc::ptr_eq(&a, &other));
    let c = map.gate_for("p", 8);
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(c.limit(), 8);
}

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
        let len = text[..head_end]
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + len {
            return;
        }
    }
}

const OK_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "m".to_string(),
        system_static: String::new(),
        system_dynamic: String::new(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    }
}

/// Two concurrent requests both get `429 Retry-After: 1`; with `max_retries = 0` they can
/// only succeed because the hint is a shared cooldown that does not use the retry budget.
#[tokio::test]
async fn retry_after_delays_all_requests_without_spending_retries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let mut served = 0usize;
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept");
            read_request(&mut socket).await;
            let resp = if served < 2 {
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string()
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{OK_BODY}"
                )
            };
            served += 1;
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    let cfg = ProviderConfig {
        base_url: format!("http://127.0.0.1:{port}"),
        ..ProviderConfig::default()
    };
    let defaults = ProviderDefaults {
        max_retries: 0,
        max_concurrent: 2,
        ..ProviderDefaults::default()
    };
    let provider = Arc::new(
        OpenAiCompat::new("gate-prov", cfg, defaults, None, BTreeMap::new()).expect("builds"),
    );

    let started = tokio::time::Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let provider = Arc::clone(&provider);
        tasks.push(tokio::spawn(async move {
            let mut stream = provider.complete(request()).await.expect("opens");
            let mut text = String::new();
            while let Some(item) = stream.next().await {
                if let StreamEvent::TextDelta { text: t } = item.expect("no error") {
                    text.push_str(&t);
                }
            }
            (text, started.elapsed())
        }));
    }
    for t in tasks {
        let (text, elapsed) = t.await.expect("joins");
        assert_eq!(text, "hi");
        assert!(elapsed >= Duration::from_millis(950), "elapsed {elapsed:?}");
    }
    assert_eq!(provider.queue_depth(), 0);
    server.abort();
}
