//! Single-flight `list_models`: concurrent cache misses share one `GET /models`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pacode_types::{ProviderConfig, ProviderDefaults};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{OpenAiCompat, Provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_concurrent_list_models_make_one_get() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let server_hits = Arc::clone(&hits);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            server_hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            // Slow enough that every caller is queued behind the first fetch.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let body = r#"{"data":[{"id":"alpha"},{"id":"beta"}]}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: format!("http://127.0.0.1:{port}"),
        api_key: None,
        api_key_env: None,
        models: vec![],
        catalog: true,
        context_window: None,
        reasoning: None,
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
        proxy: None,
    };
    let provider = Arc::new(
        OpenAiCompat::new(
            "flight",
            cfg,
            ProviderDefaults::default(),
            None,
            BTreeMap::new(),
        )
        .expect("provider builds"),
    );

    let mut tasks = Vec::new();
    for _ in 0..50 {
        let p = Arc::clone(&provider);
        tasks.push(tokio::spawn(async move { p.list_models().await }));
    }
    for t in tasks {
        let models = t.await.expect("join").expect("list_models");
        assert_eq!(models.len(), 2);
    }

    assert_eq!(hits.load(Ordering::SeqCst), 1, "exactly one catalogue GET");
    server.abort();
}
