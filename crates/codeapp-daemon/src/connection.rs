//! One client connection.

use std::sync::Arc;

use codeapp_core::Core;
use codeapp_types::SessionId;
use codeapp_types::protocol::{Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// Shared control handle: shutdown request + connection counter.
#[derive(Clone)]
pub struct ServerControl {
    pub shutdown: tokio_util::sync::CancellationToken,
    pub connections: Arc<std::sync::atomic::AtomicUsize>,
    pub app_version: String,
    pub pid: u32,
    /// Set by `Shutdown{force:false}`: exit as soon as the core is idle.
    pub shutdown_when_idle: Arc<std::sync::atomic::AtomicBool>,
}

struct ConnectionGuard {
    connections: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn send_reply(tx: &mpsc::Sender<String>, id: u64, reply: Reply) -> bool {
    let msg = ServerMessage::Reply { id, reply };
    match serde_json::to_string(&msg) {
        Ok(mut line) => {
            line.push('\n');
            tx.send(line).await.is_ok()
        }
        Err(err) => {
            log::error!("failed to serialize ServerMessage: {err}");
            false
        }
    }
}

/// Serve one connection until EOF or shutdown.
pub async fn serve_connection(stream: UnixStream, core: Arc<Core>, control: ServerControl) {
    control
        .connections
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _guard = ConnectionGuard {
        connections: Arc::clone(&control.connections),
    };

    let (reader, writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let (tx, mut rx) = mpsc::channel::<String>(1024);

    let writer_handle = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(line) = rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    let first_line = match lines.next_line().await {
        Ok(Some(line)) => line,
        Ok(None) | Err(_) => {
            drop(tx);
            let _ = writer_handle.await;
            return;
        }
    };

    let env = match serde_json::from_str::<Envelope>(first_line.trim()) {
        Ok(env) => env,
        Err(err) => {
            let _ = send_reply(
                &tx,
                0,
                Reply::Error {
                    message: format!("invalid JSON envelope: {err}"),
                },
            )
            .await;
            drop(tx);
            let _ = writer_handle.await;
            return;
        }
    };

    let hello = match env.req {
        Request::Hello(hello) => hello,
        _ => {
            let _ = send_reply(
                &tx,
                env.id,
                Reply::Error {
                    message: "first request must be Hello".to_string(),
                },
            )
            .await;
            drop(tx);
            let _ = writer_handle.await;
            return;
        }
    };

    if hello.protocol != PROTOCOL_VERSION {
        let _ = send_reply(
            &tx,
            env.id,
            Reply::Error {
                message: format!(
                    "protocol version mismatch: client protocol is {}, daemon requires {PROTOCOL_VERSION}",
                    hello.protocol
                ),
            },
        )
        .await;
        drop(tx);
        let _ = writer_handle.await;
        return;
    }

    if !send_reply(
        &tx,
        env.id,
        Reply::Hello {
            daemon_version: control.app_version.clone(),
            protocol: PROTOCOL_VERSION,
            pid: control.pid,
        },
    )
    .await
    {
        drop(tx);
        let _ = writer_handle.await;
        return;
    }

    let mut attached_session: Option<SessionId> = None;
    let mut forwarder_handle: Option<tokio::task::JoinHandle<()>> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let env = match serde_json::from_str::<Envelope>(trimmed) {
            Ok(env) => env,
            Err(err) => {
                if !send_reply(
                    &tx,
                    0,
                    Reply::Error {
                        message: format!("invalid JSON envelope: {err}"),
                    },
                )
                .await
                {
                    break;
                }
                continue;
            }
        };

        let reply = match env.req {
            Request::Hello(_) => Reply::Error {
                message: "already initialized".to_string(),
            },
            Request::Attach(attach) => {
                if let Some(handle) = forwarder_handle.take() {
                    handle.abort();
                }
                match core.open_session(attach).await {
                    Err(err) => Reply::Error {
                        message: err.to_string(),
                    },
                    Ok(session_id) => {
                        attached_session = Some(session_id.clone());
                        if let Some(mut broadcast_rx) = core.subscribe(&session_id) {
                            let event_tx = tx.clone();
                            let handle = tokio::spawn(async move {
                                loop {
                                    match broadcast_rx.recv().await {
                                        Ok((seq, event)) => {
                                            let msg = ServerMessage::Event { seq, event };
                                            if let Ok(mut s) = serde_json::to_string(&msg) {
                                                s.push('\n');
                                                let _ = event_tx.try_send(s);
                                            }
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(
                                            n,
                                        )) => {
                                            log::warn!(
                                                "session event receiver lagged by {n} events"
                                            );
                                            continue;
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                            break;
                                        }
                                    }
                                }
                            });
                            forwarder_handle = Some(handle);
                        }
                        match core.snapshot(&session_id) {
                            Some(snapshot) => Reply::Attached(snapshot),
                            None => Reply::Error {
                                message: "failed to retrieve snapshot for attached session"
                                    .to_string(),
                            },
                        }
                    }
                }
            }
            Request::Detach => {
                if let Some(handle) = forwarder_handle.take() {
                    handle.abort();
                }
                attached_session = None;
                Reply::Ok
            }
            Request::Ping => Reply::Pong,
            Request::Shutdown { force } => {
                if force {
                    control.shutdown.cancel();
                } else {
                    control
                        .shutdown_when_idle
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                Reply::Ok
            }
            Request::GetSnapshot => match &attached_session {
                Some(session_id) => match core.snapshot(session_id) {
                    Some(snapshot) => Reply::Snapshot(snapshot),
                    None => Reply::Error {
                        message: "session not found".to_string(),
                    },
                },
                None => Reply::Error {
                    message: "not attached to a session".to_string(),
                },
            },
            other_req => match &attached_session {
                Some(session_id) => core.handle(session_id, other_req).await,
                None => Reply::Error {
                    message: "not attached to a session".to_string(),
                },
            },
        };

        if !send_reply(&tx, env.id, reply).await {
            break;
        }
    }

    if let Some(handle) = forwarder_handle.take() {
        handle.abort();
    }
    drop(tx);
    let _ = writer_handle.await;
}
