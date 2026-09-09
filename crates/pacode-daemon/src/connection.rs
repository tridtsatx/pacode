//! One client connection.

use std::sync::Arc;

use pacode_core::Core;
use pacode_types::SessionId;
use pacode_types::protocol::{Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage};
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
    pub paths: pacode_config::Paths,
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
            log::warn!("protocol error: invalid first line JSON envelope: {err}");
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
            log::warn!("protocol error: first request must be Hello");
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
        log::warn!(
            "protocol version mismatch: client protocol is {}, daemon requires {PROTOCOL_VERSION}",
            hello.protocol
        );
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

    log::info!(
        "client connected: client_id={} app_version={}",
        hello.client_id,
        hello.app_version
    );

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
                log::warn!("protocol error: invalid JSON envelope: {err}");
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
                log::info!("client {} attaching: attach={attach:?}", hello.client_id);
                let _ = core.handle_global(&Request::ListMcpServers).await;
                if let Some(handle) = forwarder_handle.take() {
                    handle.abort();
                }
                // Pick up config edits (api keys, defaults) without a daemon restart.
                match pacode_config::load(&control.paths) {
                    Ok(cfg) => {
                        let mut api_keys = std::collections::BTreeMap::new();
                        for (id, pcfg) in &cfg.providers {
                            api_keys.insert(id.clone(), pacode_config::resolve_api_key(pcfg));
                        }
                        if let Err(e) = core.reload_config(cfg, &api_keys) {
                            log::warn!("config reload failed: {e}");
                        }
                    }
                    Err(e) => log::warn!("config reload failed: {e}"),
                }
                match core.open_session(attach).await {
                    Err(err) => {
                        log::warn!("client {} attach failed: {err}", hello.client_id);
                        Reply::Error {
                            message: err.to_string(),
                        }
                    }
                    Ok(session_id) => {
                        log::info!(
                            "client {} attached to session {session_id}",
                            hello.client_id
                        );
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
                log::info!(
                    "client {} detaching from session {attached_session:?}",
                    hello.client_id
                );
                let _ = core.handle_global(&Request::ListMcpServers).await;
                if let Some(handle) = forwarder_handle.take() {
                    handle.abort();
                }
                attached_session = None;
                Reply::Ok
            }
            Request::Ping => {
                let _ = core.handle_global(&Request::ListMcpServers).await;
                Reply::Pong
            }
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
                None => match core.handle_global(&other_req).await {
                    Some(reply) => reply,
                    None => Reply::Error {
                        message: "not attached to a session".to_string(),
                    },
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
    log::info!("client {} disconnected", hello.client_id);
    drop(tx);
    let _ = writer_handle.await;
}
