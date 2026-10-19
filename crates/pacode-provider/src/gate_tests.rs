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

fn hint_headers(reset: &'static str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        "x-ratelimit-reset-requests",
        reqwest::header::HeaderValue::from_static(reset),
    );
    h
}

async fn hold_n(gate: &Arc<ProviderGate>, n: usize) -> Vec<super::GatePermit> {
    let mut v = Vec::new();
    for _ in 0..n {
        v.push(gate.acquire().await);
    }
    v
}

#[tokio::test]
async fn halves_once_per_episode() {
    let gate = ProviderGate::new(64);
    let h = hint_headers("150ms");
    for _ in 0..64 {
        gate.note_response(429, &h);
    }
    assert_eq!(
        gate.effective_limit(),
        Some(32),
        "64 simultaneous 429s halve once"
    );
    assert_eq!(gate.ceiling(), Some(64));
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.note_response(503, &h);
    assert_eq!(
        gate.effective_limit(),
        Some(16),
        "a new episode halves again"
    );
}

#[tokio::test]
async fn hintless_429_shrinks_once_and_floors_at_min() {
    let gate = ProviderGate::new(64);
    let none = reqwest::header::HeaderMap::new();
    for _ in 0..10 {
        assert_eq!(gate.note_response(429, &none), None);
    }
    assert_eq!(gate.effective_limit(), Some(32));
    for _ in 0..5 {
        tokio::time::sleep(super::aimd::NO_HINT_EPISODE + Duration::from_millis(20)).await;
        gate.note_response(429, &none);
    }
    assert_eq!(gate.effective_limit(), Some(super::MIN_LIMIT));
    let tiny = ProviderGate::new(1);
    tiny.note_response(429, &none);
    assert_eq!(tiny.effective_limit(), Some(1), "never above the ceiling");
}

#[tokio::test]
async fn additive_increase_stops_at_ceiling() {
    let gate = Arc::new(ProviderGate::new(8));
    assert_eq!((gate.effective_limit(), gate.ceiling()), (Some(8), Some(8)));
    // Streams that started before the decrease and finish inside the episode: no credit.
    // (`acquire` itself waits out a cooldown, so they must be taken first.)
    let early = hold_n(&gate, 4).await;
    gate.note_response(429, &hint_headers("30ms"));
    assert_eq!(gate.effective_limit(), Some(4));
    for permit in &early {
        permit.record_success();
    }
    assert_eq!(gate.effective_limit(), Some(4));
    drop(early);

    tokio::time::sleep(Duration::from_millis(60)).await;
    for _ in 0..3 {
        gate.acquire().await.record_success();
    }
    assert_eq!(gate.effective_limit(), Some(4));
    gate.acquire().await.record_success();
    assert_eq!(gate.effective_limit(), Some(5), "+1 after L successes");
    assert_eq!(gate.available(), Some(5));

    for _ in 0..200 {
        gate.acquire().await.record_success();
    }
    assert_eq!(gate.effective_limit(), Some(8), "never above the ceiling");
    assert_eq!(gate.available(), Some(8));
    assert_eq!(gate.in_flight(), 0);
}

#[tokio::test]
async fn debt_is_paid_as_permits_return() {
    let gate = Arc::new(ProviderGate::new(8));
    let permits = hold_n(&gate, 8).await;
    let h = hint_headers("20ms");
    gate.note_response(429, &h);
    tokio::time::sleep(Duration::from_millis(40)).await;
    gate.note_response(429, &h);
    assert_eq!(gate.effective_limit(), Some(2));
    assert_eq!(gate.in_flight(), 8);
    assert_eq!(gate.available(), Some(0));

    drop(permits);
    assert_eq!(gate.available(), Some(2), "only 2 permits come back");
    assert_eq!(gate.in_flight(), 0);
    let _a = gate.acquire().await;
    let _b = gate.acquire().await;
    let third = tokio::time::timeout(SHORT, gate.acquire()).await;
    assert!(third.is_err(), "limit is 2");
}

#[tokio::test]
async fn decrease_forgets_available_permits_first() {
    let gate = Arc::new(ProviderGate::new(8));
    let permits = hold_n(&gate, 3).await;
    gate.note_response(429, &hint_headers("20ms"));
    assert_eq!(gate.effective_limit(), Some(4));
    assert_eq!(gate.available(), Some(1), "5 free, 4 forgotten");
    drop(permits);
    assert_eq!(gate.available(), Some(4), "no debt was recorded");
}

#[tokio::test]
async fn increase_pays_debt_before_adding_permits() {
    let gate = Arc::new(ProviderGate::new(8));
    let permits = hold_n(&gate, 8).await;
    gate.note_response(429, &hint_headers("20ms"));
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(gate.effective_limit(), Some(4));
    for p in permits.iter().take(4) {
        p.record_success();
    }
    drop(permits);
    assert_eq!(gate.effective_limit(), Some(5));
    assert_eq!(
        gate.available(),
        Some(5),
        "4 debt - 1 paid, 3 forgotten, 5 kept"
    );
}

#[tokio::test]
async fn unlimited_stays_unlimited_until_first_429() {
    let gate = Arc::new(ProviderGate::new(0));
    let permits = hold_n(&gate, 300).await;
    assert_eq!(gate.effective_limit(), None);
    assert_eq!(gate.ceiling(), None);
    assert_eq!(gate.available(), None);
    assert_eq!(gate.note_response(200, &hint_headers("1s")), None);
    assert_eq!(gate.effective_limit(), None);
    for p in &permits {
        p.record_success();
    }
    assert_eq!(gate.effective_limit(), None);

    gate.note_response(429, &reqwest::header::HeaderMap::new());
    assert_eq!(gate.effective_limit(), Some(super::IMPLICIT_CEILING / 2));
    assert_eq!(gate.ceiling(), Some(super::IMPLICIT_CEILING));
    assert_eq!(gate.in_flight(), 300);
    drop(permits);
    assert_eq!(gate.available(), Some(super::IMPLICIT_CEILING / 2));
    assert_eq!(gate.in_flight(), 0);
}

#[tokio::test]
async fn cancelled_waiters_during_decrease_corrupt_nothing() {
    let gate = Arc::new(ProviderGate::new(8));
    let permits = hold_n(&gate, 8).await;
    let mut waiters = Vec::new();
    for _ in 0..3 {
        let gate = Arc::clone(&gate);
        waiters.push(tokio::spawn(async move { gate.acquire().await }));
    }
    tokio::time::sleep(SHORT).await;
    assert_eq!(gate.queued(), 3);
    gate.note_response(429, &hint_headers("20ms"));
    for w in waiters {
        w.abort();
        let _ = w.await;
    }
    assert_eq!(gate.queued(), 0);
    assert_eq!(gate.in_flight(), 8);
    drop(permits);
    assert_eq!(gate.in_flight(), 0);
    assert_eq!(gate.available(), Some(4));
}

/// 64 agents x 4 requests against a provider that serves 32 concurrent streams and answers
/// 429 above that: the shared limit converges instead of herding.
#[tokio::test]
async fn converges_to_provider_capacity_without_herd() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const CAPACITY: usize = 32;
    let gate = Arc::new(ProviderGate::new(64));
    let live = Arc::new(AtomicUsize::new(0));
    let rejects = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let (gate, live, rejects) = (Arc::clone(&gate), Arc::clone(&live), Arc::clone(&rejects));
        tasks.push(tokio::spawn(async move {
            let h = hint_headers("30ms");
            for _ in 0..4 {
                loop {
                    let permit = gate.acquire().await;
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    if now > CAPACITY {
                        live.fetch_sub(1, Ordering::SeqCst);
                        rejects.fetch_add(1, Ordering::SeqCst);
                        gate.note_response(429, &h);
                        drop(permit);
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    permit.record_success();
                    live.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                    break;
                }
            }
        }));
    }
    for t in tasks {
        t.await.expect("agent joins");
    }
    let total_429 = rejects.load(Ordering::SeqCst);
    assert!(total_429 <= 2 * 256, "{total_429} 429s for 256 requests");
    let limit = gate.effective_limit().expect("limited");
    assert!(
        (8..=40).contains(&limit),
        "limit {limit} should hover near {CAPACITY}"
    );
    assert_eq!(gate.in_flight(), 0);
    assert_eq!(gate.queued(), 0);
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
