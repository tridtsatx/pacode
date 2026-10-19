//! Socket writer task: drains the connection's line queue in batches.
//!
//! One `write_all` and one `flush` per batch instead of per line (one syscall per event
//! per client was the old cost). A batch is the line that woke the task plus whatever is
//! already queued, up to [`WRITE_BATCH_BYTES`]. Nothing waits for more: an idle queue
//! flushes immediately, so latency is unchanged and nothing ticks.

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// Soft cap on one batch; a single longer line is still written whole.
pub const WRITE_BATCH_BYTES: usize = 64 * 1024;

/// Write lines from `rx` to `writer` in order until the queue closes or a write fails.
pub async fn write_batched<W: AsyncWrite + Unpin>(mut rx: mpsc::Receiver<String>, mut writer: W) {
    while let Some(mut batch) = rx.recv().await {
        while batch.len() < WRITE_BATCH_BYTES {
            match rx.try_recv() {
                Ok(line) => batch.push_str(&line),
                Err(_) => break,
            }
        }
        if writer.write_all(batch.as_bytes()).await.is_err() {
            break;
        }
        if writer.flush().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod writer_tests;
