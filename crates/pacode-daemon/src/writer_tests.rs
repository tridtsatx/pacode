use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use super::{WRITE_BATCH_BYTES, write_batched};

#[tokio::test]
async fn lines_arrive_in_order_and_complete() {
    let (tx, rx) = mpsc::channel::<String>(1024);
    let (client, mut server) = tokio::io::duplex(1 << 20);
    for i in 0..500 {
        tx.send(format!("line-{i}\n")).await.expect("queue open");
    }
    drop(tx);
    write_batched(rx, client).await;
    let mut out = String::new();
    server.read_to_string(&mut out).await.expect("read");
    let expected: String = (0..500).map(|i| format!("line-{i}\n")).collect();
    assert_eq!(out, expected);
}

#[tokio::test]
async fn oversized_line_is_written_whole() {
    let (tx, rx) = mpsc::channel::<String>(4);
    let (client, mut server) = tokio::io::duplex(1 << 20);
    let big = format!("{}\n", "x".repeat(WRITE_BATCH_BYTES * 2));
    tx.send(big.clone()).await.expect("queue open");
    tx.send("tail\n".to_string()).await.expect("queue open");
    drop(tx);
    write_batched(rx, client).await;
    let mut out = String::new();
    server.read_to_string(&mut out).await.expect("read");
    assert_eq!(out, format!("{big}tail\n"));
}
