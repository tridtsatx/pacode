use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use pacode_client::daemon_ctl::{daemon_status, spawn_daemon, stop_daemon, wait_for_socket};
use pacode_client::{Client, ClientError, ClientEvent, ClientEventContext, ClientOptions};
use pacode_config::Paths;
use pacode_types::ids::{AgentId, CallId, PermissionId, SessionId, TurnId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{
    AgentKind, AgentStatus, Mode, PermissionDecision, PermissionRequest, Plan, RiskLevel,
    SessionMeta, ToastLevel, UsageTotals,
};
use pacode_types::{
    AgentInfo, Attach, AttachEnvelope, Envelope, Event, PROTOCOL_VERSION, Reply, Request,
    ServerMessage, SessionSnapshot, SessionStatusKind, TurnStop,
};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Barrier, Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const OVERSIZED_NDJSON_LINE: usize = 8 * 1024 * 1024 + 1;

async fn write_server_message(write_half: &mut OwnedWriteHalf, message: ServerMessage) -> bool {
    let mut out = serde_json::to_string(&message).unwrap();
    out.push('\n');
    write_half.write_all(out.as_bytes()).await.is_ok() && write_half.flush().await.is_ok()
}

fn dummy_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        pending_questions: Vec::new(),
        cron_jobs: Vec::new(),
        monitors: Vec::new(),
        meta: SessionMeta {
            id: SessionId::new("ses_test"),
            name: Some("test session".into()),
            cwd: PathBuf::from("/tmp"),
            git_branch: None,
            created_at_ms: 1000,
            updated_at_ms: 1000,
            model: ModelRoute::new("dummy", "model"),
            effort: Effort::Low,
            mode: Mode::Build,
            first_prompt: None,
            zdr: false,
        },
        agents: vec![],
        plan: Plan::default(),
        tasks: vec![],
        usage: UsageTotals::default(),
        transcript: vec![],
        has_more_history: false,
        pending_permissions: vec![],
        turn_active: false,
        active_turn: None,
        terminal_turn: None,
        terminal_error: None,
        seq: 0,
    }
}

async fn handle_fake_conn(
    stream: tokio::net::UnixStream,
    ignore_next_request: Arc<AtomicBool>,
    drop_after_attach: Arc<AtomicBool>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(envelope) = serde_json::from_str::<Envelope>(trimmed) else {
                    break;
                };

                if ignore_next_request.load(Ordering::Relaxed) {
                    ignore_next_request.store(false, Ordering::Relaxed);
                    continue;
                }

                match envelope.req {
                    Request::Hello(_) => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Hello {
                                daemon_version: "t".to_string(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;
                        let _ = write_half.flush().await;
                    }
                    Request::Ping => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Pong,
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;
                        let _ = write_half.flush().await;
                    }
                    Request::Attach(_) => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Attached(dummy_snapshot()),
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;
                        let _ = write_half.flush().await;

                        if drop_after_attach.load(Ordering::Relaxed) {
                            drop_after_attach.store(false, Ordering::Relaxed);
                            break;
                        }
                    }
                    Request::UserMessage { .. } => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;

                        let ev1 = ServerMessage::Event {
                            seq: 1,
                            event: Event::Toast {
                                level: ToastLevel::Info,
                                title: "1".into(),
                                detail: None,
                            },
                        };
                        let mut out1 = serde_json::to_string(&ev1).unwrap();
                        out1.push('\n');
                        let _ = write_half.write_all(out1.as_bytes()).await;

                        let ev2 = ServerMessage::Event {
                            seq: 2,
                            event: Event::Toast {
                                level: ToastLevel::Success,
                                title: "2".into(),
                                detail: None,
                            },
                        };
                        let mut out2 = serde_json::to_string(&ev2).unwrap();
                        out2.push('\n');
                        let _ = write_half.write_all(out2.as_bytes()).await;
                        let _ = write_half.flush().await;
                    }
                    Request::Shutdown { .. } => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;
                        let _ = write_half.flush().await;
                        break;
                    }
                    _ => {
                        let reply = ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        };
                        let mut out = serde_json::to_string(&reply).unwrap();
                        out.push('\n');
                        let _ = write_half.write_all(out.as_bytes()).await;
                        let _ = write_half.flush().await;
                    }
                }
            }
            Err(_) => break,
        }
    }
}

async fn run_fake_daemon(
    listener: UnixListener,
    cancel: CancellationToken,
    ignore_next_request: Arc<AtomicBool>,
    drop_after_attach: Arc<AtomicBool>,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((stream, _)) => {
                        let ign = ignore_next_request.clone();
                        let dra = drop_after_attach.clone();
                        tokio::spawn(handle_fake_conn(stream, ign, dra));
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

async fn handle_reconnect_target_conn(
    stream: tokio::net::UnixStream,
    attach_count: Arc<AtomicUsize>,
    targets: mpsc::UnboundedSender<(u64, Attach, Option<String>)>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let Ok(wire) = serde_json::from_str::<AttachEnvelope>(line.trim()) else {
            return;
        };
        let idempotency_key = wire.idempotency_key;
        let envelope = wire.envelope;
        match envelope.req {
            Request::Hello(_) => {
                let reply = ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                };
                let mut out = serde_json::to_string(&reply).unwrap();
                out.push('\n');
                if write_half.write_all(out.as_bytes()).await.is_err() {
                    return;
                }
                let _ = write_half.flush().await;
            }
            Request::Attach(attach) => {
                let attempt = attach_count.fetch_add(1, Ordering::Relaxed);
                let _ = targets.send((envelope.id, attach, idempotency_key));
                if attempt == 0 {
                    return;
                }
                let reply = ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(dummy_snapshot()),
                };
                let mut out = serde_json::to_string(&reply).unwrap();
                out.push('\n');
                if write_half.write_all(out.as_bytes()).await.is_err() {
                    return;
                }
                let _ = write_half.flush().await;
            }
            _ => {
                let reply = ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                };
                let mut out = serde_json::to_string(&reply).unwrap();
                out.push('\n');
                if write_half.write_all(out.as_bytes()).await.is_err() {
                    return;
                }
                let _ = write_half.flush().await;
            }
        }
    }
}

async fn run_reconnect_target_daemon(
    listener: UnixListener,
    cancel: CancellationToken,
    attach_count: Arc<AtomicUsize>,
    targets: mpsc::UnboundedSender<(u64, Attach, Option<String>)>,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break; };
                let attach_count = attach_count.clone();
                let targets = targets.clone();
                tokio::spawn(handle_reconnect_target_conn(stream, attach_count, targets));
            }
        }
    }
}

async fn handle_snapshot_delivery_conn(
    stream: tokio::net::UnixStream,
    connection: usize,
    targets: mpsc::UnboundedSender<Attach>,
    flooded: mpsc::UnboundedSender<()>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let Ok(envelope) = serde_json::from_str::<Envelope>(line.trim()) else {
            return;
        };
        match envelope.req {
            Request::Hello(_) => {
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Hello {
                            daemon_version: "t".to_string(),
                            protocol: PROTOCOL_VERSION,
                            pid: 1,
                        },
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Attach(attach) => {
                if connection == 0 {
                    if !write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Attached(dummy_snapshot()),
                        },
                    )
                    .await
                    {
                        return;
                    }
                    for seq in 1..=1100 {
                        let event = ServerMessage::Event {
                            seq,
                            event: Event::TextDelta {
                                agent: AgentId::main(),
                                item_seq: seq,
                                text: format!("flood_{seq}"),
                            },
                        };
                        if !write_server_message(&mut write_half, event).await {
                            return;
                        }
                    }
                    let _ = flooded.send(());
                    return;
                }
                let _ = targets.send(attach);
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Attached(dummy_snapshot()),
                    },
                )
                .await
                {
                    return;
                }
            }
            _ => {
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    },
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn run_snapshot_delivery_daemon(
    listener: UnixListener,
    cancel: CancellationToken,
    targets: mpsc::UnboundedSender<Attach>,
    flooded: mpsc::UnboundedSender<()>,
) {
    let connections = Arc::new(AtomicUsize::new(0));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break; };
                let connection = connections.fetch_add(1, Ordering::Relaxed);
                let targets = targets.clone();
                let flooded = flooded.clone();
                tokio::spawn(handle_snapshot_delivery_conn(
                    stream,
                    connection,
                    targets,
                    flooded,
                ));
            }
        }
    }
}

async fn handle_detach_reconnect_conn(
    stream: tokio::net::UnixStream,
    connection: usize,
    requests: mpsc::UnboundedSender<bool>,
    reconnected: mpsc::UnboundedSender<()>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let Ok(read) = reader.read_line(&mut line).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let Ok(envelope) = serde_json::from_str::<Envelope>(line.trim()) else {
            return;
        };
        match envelope.req {
            Request::Hello(_) => {
                if connection == 1 {
                    let _ = reconnected.send(());
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Hello {
                            daemon_version: "t".to_string(),
                            protocol: PROTOCOL_VERSION,
                            pid: 1,
                        },
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Attach(_) => {
                if connection == 1 {
                    let _ = requests.send(true);
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Attached(dummy_snapshot()),
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Detach => {
                if connection == 0 {
                    return;
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Ping => {
                if connection == 1 {
                    let _ = requests.send(false);
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Pong,
                    },
                )
                .await
                {
                    return;
                }
            }
            _ => {
                if connection == 1 {
                    let _ = requests.send(false);
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    },
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn run_detach_reconnect_daemon(
    listener: UnixListener,
    cancel: CancellationToken,
    requests: mpsc::UnboundedSender<bool>,
    reconnected: mpsc::UnboundedSender<()>,
) {
    let connections = Arc::new(AtomicUsize::new(0));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break; };
                let connection = connections.fetch_add(1, Ordering::Relaxed);
                let requests = requests.clone();
                let reconnected = reconnected.clone();
                tokio::spawn(handle_detach_reconnect_conn(
                    stream,
                    connection,
                    requests,
                    reconnected,
                ));
            }
        }
    }
}

async fn handle_stale_detach_reconnect_conn(
    stream: tokio::net::UnixStream,
    connection: usize,
    attach_started: mpsc::UnboundedSender<()>,
    mut release_attach: Option<mpsc::UnboundedReceiver<()>>,
    rollback_seen: mpsc::UnboundedSender<()>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let wire: AttachEnvelope = match serde_json::from_str(line.trim()) {
            Ok(wire) => wire,
            Err(_) => return,
        };
        match wire.envelope.req {
            Request::Hello(_) => {
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: wire.envelope.id,
                        reply: Reply::Hello {
                            daemon_version: "t".into(),
                            protocol: PROTOCOL_VERSION,
                            pid: 1,
                        },
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Attach(_) => {
                if connection == 1 {
                    let _ = attach_started.send(());
                    let Some(mut release) = release_attach.take() else {
                        return;
                    };
                    if release.recv().await.is_none() {
                        return;
                    }
                }
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: wire.envelope.id,
                        reply: Reply::Attached(dummy_snapshot()),
                    },
                )
                .await
                {
                    return;
                }
                if connection == 0 {
                    return;
                }
            }
            Request::Detach => {
                if connection != 1 {
                    return;
                }
                let _ = rollback_seen.send(());
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: wire.envelope.id,
                        reply: Reply::Ok,
                    },
                )
                .await
                {
                    return;
                }
            }
            Request::Ping => {
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: wire.envelope.id,
                        reply: Reply::Pong,
                    },
                )
                .await
                {
                    return;
                }
            }
            _ => {
                if !write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: wire.envelope.id,
                        reply: Reply::Ok,
                    },
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn run_stale_detach_reconnect_daemon(
    listener: UnixListener,
    cancel: CancellationToken,
    attach_started: mpsc::UnboundedSender<()>,
    rollback_seen: mpsc::UnboundedSender<()>,
    release_attach: mpsc::UnboundedReceiver<()>,
) {
    let mut connection = 0usize;
    let mut release_attach = Some(release_attach);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break; };
                let current = connection;
                connection += 1;
                let release = if current == 1 { release_attach.take() } else { None };
                tokio::spawn(handle_stale_detach_reconnect_conn(
                    stream,
                    current,
                    attach_started.clone(),
                    release,
                    rollback_seen.clone(),
                ));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum DelayedAttachReply {
    Attached,
    Error,
}

async fn handle_retry_attach_conn(
    stream: tokio::net::UnixStream,
    first_seen: mpsc::UnboundedSender<(u64, Attach, Option<String>)>,
    second_seen: mpsc::UnboundedSender<(u64, Attach, Option<String>)>,
    created_sessions: Arc<AtomicUsize>,
    delayed_reply: DelayedAttachReply,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut assigned: Option<(String, SessionId)> = None;
    let mut first_request_id = None;
    let mut attach_count = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let wire: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
        let idempotency_key = wire.idempotency_key;
        let envelope = wire.envelope;
        match envelope.req {
            Request::Hello(_) => {
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Hello {
                                daemon_version: "t".into(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        },
                    )
                    .await
                );
            }
            Request::Attach(attach) => {
                attach_count += 1;
                let idempotency_key = idempotency_key.expect("identified attach missing key");
                let session_id = if let Some((assigned_key, session_id)) = &assigned
                    && assigned_key == &idempotency_key
                {
                    session_id.clone()
                } else {
                    let session_id = SessionId::new("ses_retry_stable");
                    created_sessions.fetch_add(1, Ordering::Relaxed);
                    assigned = Some((idempotency_key.clone(), session_id.clone()));
                    session_id
                };
                if attach_count == 1 {
                    first_request_id = Some(envelope.id);
                    let _ = first_seen.send((envelope.id, attach, Some(idempotency_key)));
                    continue;
                }

                let _ = second_seen.send((envelope.id, attach, Some(idempotency_key)));
                let delayed_reply = match delayed_reply {
                    DelayedAttachReply::Attached => {
                        let mut snapshot = dummy_snapshot();
                        snapshot.meta.id = SessionId::new("ses_retry_old");
                        Reply::Attached(snapshot)
                    }
                    DelayedAttachReply::Error => Reply::Error {
                        message: "late first attach failed".into(),
                    },
                };
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: first_request_id.unwrap(),
                            reply: delayed_reply,
                        },
                    )
                    .await
                );
                let mut snapshot = dummy_snapshot();
                snapshot.meta.id = session_id;
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Attached(snapshot),
                        },
                    )
                    .await
                );
                break;
            }
            _ => {
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        },
                    )
                    .await
                );
            }
        }
    }
}

async fn handle_sequence_order_conn(
    stream: tokio::net::UnixStream,
    first_seen: mpsc::UnboundedSender<u64>,
    second_seen: mpsc::UnboundedSender<u64>,
    second_before_release: mpsc::UnboundedSender<()>,
    fail_first: bool,
    mut release: mpsc::UnboundedReceiver<()>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut sequence_count = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        match envelope.req {
            Request::Hello(_) => {
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Hello {
                                daemon_version: "t".into(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        },
                    )
                    .await
                );
            }
            Request::Attach(_) => {
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Attached(dummy_snapshot()),
                        },
                    )
                    .await
                );
            }
            Request::UserMessage { .. } => {
                sequence_count += 1;
                if sequence_count == 1 {
                    let _ = first_seen.send(envelope.id);
                    let mut second_line = String::new();
                    let (released, early_second) = tokio::select! {
                        _ = release.recv() => (true, None),
                        result = reader.read_line(&mut second_line) => {
                            let request = if result.unwrap_or(0) == 0 {
                                None
                            } else {
                                Some(serde_json::from_str::<Envelope>(second_line.trim()).unwrap())
                            };
                            (false, request)
                        }
                    };
                    if !released {
                        let _ = release.recv().await;
                    }
                    if early_second.is_some() {
                        let _ = second_before_release.send(());
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: if fail_first {
                                    Reply::Error {
                                        message: "first failed".into(),
                                    }
                                } else {
                                    Reply::Ok
                                },
                            },
                        )
                        .await
                    );
                    if let Some(second) = early_second {
                        let _ = second_seen.send(second.id);
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Reply {
                                    id: second.id,
                                    reply: Reply::Ok,
                                },
                            )
                            .await
                        );
                    }
                } else {
                    let _ = second_seen.send(envelope.id);
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                }
            }
            _ => {
                assert!(
                    write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        },
                    )
                    .await
                );
            }
        }
    }
}

async fn run_sequence_order_daemon(
    listener: UnixListener,
    first_seen: mpsc::UnboundedSender<u64>,
    second_seen: mpsc::UnboundedSender<u64>,
    second_before_release: mpsc::UnboundedSender<()>,
    fail_first: bool,
    release: mpsc::UnboundedReceiver<()>,
) {
    let (stream, _) = listener.accept().await.unwrap();
    handle_sequence_order_conn(
        stream,
        first_seen,
        second_seen,
        second_before_release,
        fail_first,
        release,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reconnect_uses_desired_new_attach() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();
    let attach_count = Arc::new(AtomicUsize::new(0));
    let (target_tx, mut target_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_reconnect_target_daemon(
        listener,
        cancel.clone(),
        attach_count.clone(),
        target_tx,
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));

    let result = client
        .attach(Attach::New {
            cwd: PathBuf::from("/tmp"),
            model: None,
            effort: None,
            mode: None,
        })
        .await;
    assert!(matches!(result, Err(ClientError::Disconnected)));
    let (initial_id, initial_target, initial_key) = target_rx.recv().await.unwrap();
    assert!(matches!(initial_target, Attach::New { .. }));
    assert!(initial_key.is_some());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Snapshot { .. }) => break,
                Some(_) => {}
                None => panic!("event stream closed before reconnect snapshot"),
            }
        }
    })
    .await
    .unwrap();
    let (retry_id, retry_target, retry_key) =
        tokio::time::timeout(Duration::from_secs(5), target_rx.recv())
            .await
            .unwrap()
            .unwrap();
    assert_ne!(retry_id, initial_id);
    assert_eq!(retry_target, initial_target);
    assert_eq!(retry_key, initial_key);

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_session_request_is_scoped_and_clears_attach() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("close-session.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            let reply = match envelope.req {
                Request::Hello(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".into(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
                Request::Attach(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(dummy_snapshot()),
                },
                Request::CloseSession => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                    break;
                }
                _ => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
            };
            assert!(write_server_message(&mut write_half, reply).await);
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.close_session().await.unwrap(), Reply::Ok);
    assert!(client.session().is_none());
    drop(client);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_attach_requests_are_serialized_before_side_effects() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("serialized-attach.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_seen_tx, mut first_seen_rx) = mpsc::unbounded_channel();
    let (second_seen_tx, mut second_seen_rx) = mpsc::unbounded_channel();
    let (release_first_tx, mut release_first_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut attach_count = 0usize;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let wire: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
            let envelope = wire.envelope;
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(attach) => {
                    attach_count += 1;
                    if attach_count == 1 {
                        let _ = first_seen_tx.send(attach);
                        release_first_rx.recv().await.unwrap();
                    } else {
                        let _ = second_seen_tx.send(attach);
                    }
                    let mut snapshot = dummy_snapshot();
                    snapshot.meta.id = SessionId::new(if attach_count == 1 {
                        "ses_first"
                    } else {
                        "ses_second"
                    });
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(snapshot),
                            },
                        )
                        .await
                    );
                }
                _ => return,
            }
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let client = Arc::new(client);
    let first_client = Arc::clone(&client);
    let first_task = tokio::spawn(async move {
        first_client
            .attach(Attach::New {
                cwd: PathBuf::from("/tmp/first"),
                model: None,
                effort: None,
                mode: None,
            })
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), first_seen_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        Attach::New {
            cwd: PathBuf::from("/tmp/first"),
            model: None,
            effort: None,
            mode: None,
        }
    );
    let (second_started_tx, second_started_rx) = oneshot::channel();
    let second_client = Arc::clone(&client);
    let second_task = tokio::spawn(async move {
        let _ = second_started_tx.send(());
        second_client
            .attach(Attach::Latest {
                cwd: PathBuf::from("/tmp/second"),
            })
            .await
    });
    second_started_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_seen_rx.recv())
            .await
            .is_err()
    );
    release_first_tx.send(()).unwrap();
    assert_eq!(
        first_task.await.unwrap().unwrap().meta.id,
        SessionId::new("ses_first")
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), second_seen_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        Attach::Latest {
            cwd: PathBuf::from("/tmp/second"),
        }
    );
    assert_eq!(
        second_task.await.unwrap().unwrap().meta.id,
        SessionId::new("ses_second")
    );
    assert_eq!(client.session(), Some(SessionId::new("ses_second")));

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn close_waits_until_transport_is_shut_down() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("close-transport.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (closed_tx, mut closed_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
        assert!(
            write_server_message(
                &mut write_half,
                ServerMessage::Reply {
                    id: hello.id,
                    reply: Reply::Hello {
                        daemon_version: "t".into(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
            )
            .await
        );
        line.clear();
        assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
        let _ = closed_tx.send(());
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));

    tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), closed_rx.recv())
        .await
        .unwrap()
        .unwrap();
    daemon_task.await.unwrap();
}

async fn assert_retry_attach_ignores_delayed_reply(delayed_reply: DelayedAttachReply) {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("retry-attach.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let created_sessions = Arc::new(AtomicUsize::new(0));
    let daemon_created_sessions = created_sessions.clone();
    let daemon_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_retry_attach_conn(
            stream,
            first_tx,
            second_tx,
            daemon_created_sessions,
            delayed_reply,
        )
        .await;
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    opts.request_timeout = Duration::from_millis(50);
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let result = client.attach(attach.clone()).await;
    assert!(matches!(result, Err(ClientError::Timeout("attach"))));
    let (first_id, first_target, first_key) =
        tokio::time::timeout(Duration::from_secs(1), first_rx.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(first_target, attach);

    let snapshot = {
        let retry = client.retry_attach(attach.clone());
        tokio::pin!(retry);
        let (second_id, second_target, second_key) = tokio::select! {
            biased;
            second = tokio::time::timeout(Duration::from_secs(1), second_rx.recv()) => second.unwrap().unwrap(),
            result = &mut retry => panic!("retry completed before its request was observed: {result:?}"),
        };
        assert_ne!(second_id, first_id);
        assert_eq!(second_target, attach);
        assert_eq!(second_key, first_key);
        tokio::time::timeout(Duration::from_secs(1), retry)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(snapshot.meta.id, SessionId::new("ses_retry_stable"));
    assert_eq!(created_sessions.load(Ordering::Relaxed), 1);
    assert!(matches!(
        client
            .retry_attach(Attach::Resume {
                session: snapshot.meta.id.clone(),
            })
            .await,
        Err(ClientError::StaleSession)
    ));

    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_attach_ignores_delayed_first_attached() {
    assert_retry_attach_ignores_delayed_reply(DelayedAttachReply::Attached).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_attach_ignores_delayed_first_error() {
    assert_retry_attach_ignores_delayed_reply(DelayedAttachReply::Error).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reconnect_snapshot_survives_full_event_queue() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("snapshot-full.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();
    let (target_tx, mut target_rx) = mpsc::unbounded_channel();
    let (flooded_tx, mut flooded_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_snapshot_delivery_daemon(
        listener,
        cancel.clone(),
        target_tx,
        flooded_tx,
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    client
        .attach(Attach::New {
            cwd: PathBuf::from("/tmp"),
            model: None,
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), flooded_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Snapshot { snapshot, .. }) => break snapshot,
                Some(_) => {}
                None => panic!("event stream closed before reconnect snapshot"),
            }
        }
    })
    .await
    .unwrap();
    let target = tokio::time::timeout(Duration::from_secs(5), target_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        target,
        Attach::Resume {
            session: SessionId::new("ses_test")
        }
    );
    assert_eq!(snapshot.meta.id, SessionId::new("ses_test"));

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_detach_before_ack_does_not_reconnect_to_session() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("detach-reconnect.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (reconnected_tx, mut reconnected_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_detach_reconnect_daemon(
        listener,
        cancel.clone(),
        request_tx,
        reconnected_tx,
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.session(), Some(SessionId::new("ses_test")));
    assert!(matches!(
        client.request(Request::Detach).await,
        Err(ClientError::Disconnected)
    ));
    assert_eq!(client.session(), None);

    tokio::time::timeout(Duration::from_secs(5), reconnected_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let mut snapshot_seen = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Connected { context, .. }) if context.connection_epoch == 2 => {
                    break;
                }
                Some(ClientEvent::Snapshot { .. }) => snapshot_seen = true,
                Some(_) => {}
                None => panic!("event stream closed before reconnect"),
            }
        }
    })
    .await
    .unwrap();
    assert!(!snapshot_seen);
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);
    let first_request = tokio::time::timeout(Duration::from_secs(1), request_rx.recv())
        .await
        .unwrap();
    assert!(!first_request.unwrap());

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_stale_detach_rolls_back_server_attachment() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("stale-detach-reconnect.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();
    let (attach_started_tx, mut attach_started_rx) = mpsc::unbounded_channel();
    let (rollback_seen_tx, mut rollback_seen_rx) = mpsc::unbounded_channel();
    let (release_attach_tx, release_attach_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_stale_detach_reconnect_daemon(
        listener,
        cancel.clone(),
        attach_started_tx,
        rollback_seen_tx,
        release_attach_rx,
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), attach_started_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let client = Arc::new(client);
    let detach_client = Arc::clone(&client);
    let detach_task = tokio::spawn(async move { detach_client.request(Request::Detach).await });
    assert!(matches!(
        detach_task.await.unwrap(),
        Err(ClientError::Disconnected)
    ));
    assert_eq!(client.session(), None);
    release_attach_tx.send(()).unwrap();

    tokio::time::timeout(Duration::from_secs(5), rollback_seen_rx.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Connected { context, .. }) if context.connection_epoch == 2 => {
                    break;
                }
                Some(ClientEvent::Snapshot { .. }) => panic!("stale reconnect snapshot"),
                Some(_) => {}
                None => panic!("event stream closed before rollback reconnect"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(client.session(), None);
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_session_request_is_rejected_during_switch() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("generic-switch.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (attach_started_tx, mut attach_started_rx) = mpsc::unbounded_channel();
    let (release_attach_tx, mut release_attach_rx) = mpsc::unbounded_channel();
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut attach_count = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    attach_count += 1;
                    if attach_count == 2 {
                        let _ = attach_started_tx.send(());
                        release_attach_rx.recv().await.unwrap();
                    }
                    let mut snapshot = dummy_snapshot();
                    if attach_count == 2 {
                        snapshot.meta.id = SessionId::new("ses_other");
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.envelope.id,
                                reply: Reply::Attached(snapshot),
                            },
                        )
                        .await
                    );
                }
                Request::Detach => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                }
                request => {
                    let _ = request_tx.send(request);
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                }
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();

    let client = Arc::new(client);
    let switch_client = Arc::clone(&client);
    let switch_task = tokio::spawn(async move {
        switch_client
            .switch_session(Attach::Resume {
                session: SessionId::new("ses_other"),
            })
            .await
    });
    attach_started_rx.recv().await.unwrap();
    for request in [
        Request::UserMessage {
            text: "must not cross switch".into(),
            images: Vec::new(),
            message_id: None,
            attachments: Vec::new(),
        },
        Request::Interrupt,
        Request::GetSnapshot,
    ] {
        assert!(matches!(
            client.request(request).await,
            Err(ClientError::StaleSession)
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request_rx.recv())
            .await
            .is_err()
    );
    release_attach_tx.send(()).unwrap();
    switch_task.await.unwrap().unwrap();
    client
        .request_session_for(client.event_context(), Request::Interrupt)
        .await
        .unwrap();
    assert!(matches!(request_rx.recv().await, Some(Request::Interrupt)));

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_session_request_is_not_sent_after_attach_transition() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("stale-session.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (attach_started_tx, mut attach_started_rx) = mpsc::unbounded_channel();
    let (release_attach_tx, mut release_attach_rx) = mpsc::unbounded_channel();
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut attach_count = 0usize;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    attach_count += 1;
                    if attach_count == 2 {
                        let _ = attach_started_tx.send(());
                        release_attach_rx.recv().await.unwrap();
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                }
                request => {
                    let _ = request_tx.send(request);
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                }
            }
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let old_context = client.event_context();
    let client = Arc::new(client);
    let attach_client = Arc::clone(&client);
    let attach_task = tokio::spawn(async move {
        attach_client
            .attach(Attach::Resume {
                session: SessionId::new("ses_test"),
            })
            .await
    });
    attach_started_rx.recv().await.unwrap();
    let stale_client = Arc::clone(&client);
    let stale_task = tokio::spawn(async move {
        stale_client
            .request_sequence(
                old_context,
                [
                    Request::UserMessage {
                        text: "stale-1".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                    Request::UserMessage {
                        text: "stale-2".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                ],
            )
            .await
    });
    tokio::task::yield_now().await;
    release_attach_tx.send(()).unwrap();
    attach_task.await.unwrap().unwrap();
    assert!(matches!(
        stale_task.await.unwrap(),
        Err(ClientError::StaleSession)
    ));
    client
        .request_session_for(client.event_context(), Request::Interrupt)
        .await
        .unwrap();
    assert!(matches!(request_rx.recv().await, Some(Request::Interrupt)));

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn request_sequence_waits_for_each_reply() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("sequence-order.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let (second_before_tx, mut second_before_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_sequence_order_daemon(
        listener,
        first_tx,
        second_tx,
        second_before_tx,
        false,
        release_rx,
    ));
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let context = client.event_context();
    let client = Arc::new(client);
    let sequence_client = Arc::clone(&client);
    let sequence_task = tokio::spawn(async move {
        sequence_client
            .request_sequence(
                context,
                [
                    Request::UserMessage {
                        text: "first".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                    Request::UserMessage {
                        text: "second".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                ],
            )
            .await
    });
    let first_id = tokio::time::timeout(Duration::from_secs(1), first_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_before_rx.recv())
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    let replies = tokio::time::timeout(Duration::from_secs(1), sequence_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second_id = tokio::time::timeout(Duration::from_secs(1), second_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replies, vec![Reply::Ok, Reply::Ok]);
    assert!(first_id > 2);
    assert_eq!(second_id, first_id + 1);

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn request_sequence_stops_after_timeout() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("sequence-timeout.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let (second_before_tx, mut second_before_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_sequence_order_daemon(
        listener,
        first_tx,
        second_tx,
        second_before_tx,
        false,
        release_rx,
    ));
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    opts.request_timeout = Duration::from_millis(40);
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let context = client.event_context();
    let client = Arc::new(client);
    let sequence_client = Arc::clone(&client);
    let sequence_task = tokio::spawn(async move {
        sequence_client
            .request_sequence(
                context,
                [
                    Request::UserMessage {
                        text: "first".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                    Request::UserMessage {
                        text: "second".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                ],
            )
            .await
    });
    let _ = tokio::time::timeout(Duration::from_secs(1), first_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), sequence_task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(ClientError::Timeout("user_message"))));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_before_rx.recv())
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_rx.recv())
            .await
            .is_err()
    );

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn request_sequence_stops_after_reply_error() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("sequence-error.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let (second_before_tx, mut second_before_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(run_sequence_order_daemon(
        listener,
        first_tx,
        second_tx,
        second_before_tx,
        true,
        release_rx,
    ));
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let context = client.event_context();
    let client = Arc::new(client);
    let sequence_client = Arc::clone(&client);
    let sequence_task = tokio::spawn(async move {
        sequence_client
            .request_sequence(
                context,
                [
                    Request::UserMessage {
                        text: "first".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                    Request::UserMessage {
                        text: "second".into(),
                        images: Vec::new(),
                        message_id: None,
                        attachments: Vec::new(),
                    },
                ],
            )
            .await
    });
    let _ = tokio::time::timeout(Duration::from_secs(1), first_rx.recv())
        .await
        .unwrap()
        .unwrap();
    release_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), sequence_task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(ClientError::Daemon(message)) if message == "first failed"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_before_rx.recv())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_rx.recv())
            .await
            .is_err()
    );

    let client = Arc::try_unwrap(client).ok().unwrap();
    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connect_and_connected_event() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();

    let daemon_task = tokio::spawn(run_fake_daemon(
        listener,
        cancel.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, mut rx) = Client::connect(opts).await.unwrap();
    assert!(client.is_connected());

    let ev = rx.recv().await.unwrap();
    assert_eq!(
        ev,
        ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 0,
                session: None,
            },
            daemon_version: "t".into(),
            pid: 1,
            version_mismatch: false,
        }
    );

    client.close().await;
    assert!(rx.recv().await.is_none());
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_ping_pong() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();

    let daemon_task = tokio::spawn(run_fake_daemon(
        listener,
        cancel.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, _rx) = Client::connect(opts).await.unwrap();
    let reply = client.request(Request::Ping).await.unwrap();
    assert_eq!(reply, Reply::Pong);

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_attach_snapshot_and_events_order() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();

    let daemon_task = tokio::spawn(run_fake_daemon(
        listener,
        cancel.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, mut rx) = Client::connect(opts).await.unwrap();
    let conn_event = rx.recv().await.unwrap();
    assert!(matches!(conn_event, ClientEvent::Connected { .. }));

    let snapshot = client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.meta.id, SessionId::new("ses_test"));
    assert_eq!(client.session(), Some(SessionId::new("ses_test")));

    client
        .ok(Request::UserMessage {
            text: "hello".into(),
            images: Vec::new(),
            message_id: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();

    let ev1 = rx.recv().await.unwrap();
    assert_eq!(
        ev1,
        ClientEvent::Event {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            seq: 1,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "1".into(),
                detail: None,
            }
        }
    );

    let ev2 = rx.recv().await.unwrap();
    assert_eq!(
        ev2,
        ClientEvent::Event {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            seq: 2,
            event: Event::Toast {
                level: ToastLevel::Success,
                title: "2".into(),
                detail: None,
            }
        }
    );

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_timeout() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();

    let ignore_next = Arc::new(AtomicBool::new(false));
    let daemon_task = tokio::spawn(run_fake_daemon(
        listener,
        cancel.clone(),
        ignore_next.clone(),
        Arc::new(AtomicBool::new(false)),
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    opts.request_timeout = Duration::from_millis(50);

    let (client, _rx) = Client::connect(opts).await.unwrap();

    ignore_next.store(true, Ordering::Relaxed);
    let res = client.request(Request::Ping).await;
    assert!(matches!(res, Err(ClientError::Timeout("ping"))));

    client.close().await;
    cancel.cancel();
    let _ = daemon_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reconnect() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel1 = CancellationToken::new();

    let drop_after_attach = Arc::new(AtomicBool::new(true));
    let daemon_task1 = tokio::spawn(run_fake_daemon(
        listener,
        cancel1.clone(),
        Arc::new(AtomicBool::new(false)),
        drop_after_attach,
    ));

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket.clone());
    opts.spawn_daemon = false;

    let (client, mut rx) = Client::connect(opts).await.unwrap();
    let initial_conn = rx.recv().await.unwrap();
    assert!(matches!(initial_conn, ClientEvent::Connected { .. }));

    // Attach causes fake daemon 1 to drop the connection
    let _snapshot = client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();

    // Client detects EOF and starts reconnecting
    let disc = rx.recv().await.unwrap();
    assert!(matches!(disc, ClientEvent::Disconnected { .. }));

    let reconn = rx.recv().await.unwrap();
    assert!(matches!(
        reconn,
        ClientEvent::Reconnecting { attempt: 1, .. }
    ));

    cancel1.cancel();
    let _ = daemon_task1.await;

    // Start fake daemon 2
    let _ = std::fs::remove_file(&socket);
    let listener2 = UnixListener::bind(&socket).unwrap();
    let cancel2 = CancellationToken::new();
    let daemon_task2 = tokio::spawn(run_fake_daemon(
        listener2,
        cancel2.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ));

    // Client reconnects: receives Connected and Snapshot
    loop {
        match rx.recv().await.unwrap() {
            ClientEvent::Connected { .. } => break,
            ClientEvent::Reconnecting { .. } => {}
            other => panic!("expected Connected before Snapshot, got {other:?}"),
        }
    }

    loop {
        match rx.recv().await.unwrap() {
            ClientEvent::Snapshot { .. } => break,
            ClientEvent::Reconnecting { .. } => {}
            other => panic!("expected Snapshot after Connected, got {other:?}"),
        }
    }

    assert!(client.is_connected());
    let pong = client.request(Request::Ping).await.unwrap();
    assert_eq!(pong, Reply::Pong);

    client.close().await;
    cancel2.cancel();
    let _ = daemon_task2.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_attach_session_not_found_is_terminal() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("reconnect-missing.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cancel = CancellationToken::new();
    let daemon_cancel = cancel.clone();
    let reconnect_attach_count = Arc::new(AtomicUsize::new(0));
    let daemon_attach_count = Arc::clone(&reconnect_attach_count);
    let (seen_tx, mut seen_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let mut connection = 0usize;
        loop {
            tokio::select! {
                _ = daemon_cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    let current = connection;
                    connection += 1;
                    let seen_tx = seen_tx.clone();
                    let reconnect_attach_count = Arc::clone(&daemon_attach_count);
                    tokio::spawn(async move {
                        let (read_half, mut write_half) = stream.into_split();
                        let mut reader = BufReader::new(read_half);
                        let mut line = String::new();
                        loop {
                            line.clear();
                            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            let wire: AttachEnvelope = match serde_json::from_str(line.trim()) {
                                Ok(wire) => wire,
                                Err(_) => return,
                            };
                            let envelope = wire.envelope;
                            match envelope.req {
                                Request::Hello(_) => {
                                    if !write_server_message(
                                        &mut write_half,
                                        ServerMessage::Reply {
                                            id: envelope.id,
                                            reply: Reply::Hello {
                                                daemon_version: "t".into(),
                                                protocol: PROTOCOL_VERSION,
                                                pid: 1,
                                            },
                                        },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }
                                Request::Attach(attach) => {
                                    let snapshot_id = match &attach {
                                        Attach::Resume { session } => Some(session.clone()),
                                        Attach::New { .. } | Attach::Latest { .. } => None,
                                    };
                                    let _ = seen_tx.send((
                                        current,
                                        envelope.id,
                                        attach.clone(),
                                        wire.idempotency_key.clone(),
                                    ));
                                    if current == 0 {
                                        let mut snapshot = dummy_snapshot();
                                        if let Some(session) = snapshot_id {
                                            snapshot.meta.id = session;
                                        }
                                        if !write_server_message(
                                            &mut write_half,
                                            ServerMessage::Reply {
                                                id: envelope.id,
                                                reply: Reply::Attached(snapshot),
                                            },
                                        )
                                        .await
                                        {
                                            return;
                                        }
                                        return;
                                    }
                                    reconnect_attach_count.fetch_add(1, Ordering::Relaxed);
                                    if !write_server_message(
                                        &mut write_half,
                                        ServerMessage::Reply {
                                            id: envelope.id,
                                            reply: Reply::Error {
                                                message: "session not found".into(),
                                            },
                                        },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                    return;
                                }
                                _ => {
                                    if !write_server_message(
                                        &mut write_half,
                                        ServerMessage::Reply {
                                            id: envelope.id,
                                            reply: Reply::Ok,
                                        },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    });
                }
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let target = Attach::Resume {
        session: SessionId::new("ses_missing"),
    };
    client.attach(target.clone()).await.unwrap();

    let (initial_connection, initial_id, initial_target, initial_key) =
        tokio::time::timeout(Duration::from_secs(5), seen_rx.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(initial_connection, 0);
    assert_eq!(initial_target, target);
    let (reconnect_connection, reconnect_id, reconnect_target, reconnect_key) =
        tokio::time::timeout(Duration::from_secs(5), seen_rx.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(reconnect_connection, 1);
    assert_eq!(reconnect_target, target);
    assert_ne!(reconnect_id, initial_id);
    assert_eq!(reconnect_key, initial_key);

    let failure = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::AttachFailed {
                    context,
                    attach,
                    reason,
                }) => break (context, attach, reason),
                Some(_) => {}
                None => panic!("event stream closed before attach failure"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(failure.0.session, None);
    assert_eq!(failure.1, target);
    assert_eq!(failure.2, "session not found");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Connected { context, .. }) if context.session.is_none() => break,
                Some(_) => {}
                None => panic!("event stream closed before detached reconnect"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reconnect_attach_count.load(Ordering::Relaxed), 1);
    assert!(client.session().is_none());

    client.close().await;
    cancel.cancel();
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_buffers_events_and_filters_snapshot_seq() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("reconnect-order.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon_task = tokio::spawn(async move {
        let mut connection = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let current = connection;
            connection += 1;
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                    match envelope.req {
                        Request::Hello(_) => {
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Hello {
                                            daemon_version: "t".into(),
                                            protocol: PROTOCOL_VERSION,
                                            pid: 1,
                                        },
                                    },
                                )
                                .await
                            );
                        }
                        Request::Attach(_) => {
                            let mut snapshot = dummy_snapshot();
                            snapshot.seq = 10;
                            if current != 0 {
                                assert!(
                                    write_server_message(
                                        &mut write_half,
                                        ServerMessage::Event {
                                            seq: 1,
                                            event: Event::SessionStatus {
                                                session: SessionId::new("ses_other"),
                                                status: SessionStatusKind::Done,
                                            },
                                        },
                                    )
                                    .await
                                );
                            }
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Attached(snapshot),
                                    },
                                )
                                .await
                            );
                            if current == 0 {
                                return;
                            }
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Event {
                                        seq: 10,
                                        event: Event::Toast {
                                            level: ToastLevel::Info,
                                            title: "stale".into(),
                                            detail: None,
                                        },
                                    },
                                )
                                .await
                            );
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Event {
                                        seq: 11,
                                        event: Event::Toast {
                                            level: ToastLevel::Info,
                                            title: "fresh".into(),
                                            detail: None,
                                        },
                                    },
                                )
                                .await
                            );
                            return;
                        }
                        _ => return,
                    }
                }
            });
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();

    let mut saw_global = false;
    let mut saw_snapshot = false;
    let mut saw_fresh = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                ClientEvent::Connected { context, .. } if context.connection_epoch == 2 => {}
                ClientEvent::Event {
                    event: Event::SessionStatus { status, .. },
                    ..
                } => {
                    assert_eq!(status, SessionStatusKind::Done);
                    saw_global = true;
                }
                ClientEvent::Snapshot { snapshot, .. } => {
                    assert_eq!(snapshot.seq, 10);
                    saw_snapshot = true;
                }
                ClientEvent::Event { seq: 11, .. } => saw_fresh = true,
                ClientEvent::Event { seq: 10, .. } => panic!("stale snapshot event was delivered"),
                _ if saw_fresh => break,
                _ => {}
            }
            if saw_global && saw_snapshot && saw_fresh {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_global && saw_snapshot && saw_fresh);

    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_hello_and_attach_handshakes_time_out() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("handshake-timeout.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon_task = tokio::spawn(async move {
        let mut connection = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let current = connection;
            connection += 1;
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                    match envelope.req {
                        Request::Hello(_) if current == 1 => {
                            std::future::pending::<()>().await;
                        }
                        Request::Hello(_) => {
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Hello {
                                            daemon_version: "t".into(),
                                            protocol: PROTOCOL_VERSION,
                                            pid: 1,
                                        },
                                    },
                                )
                                .await
                            );
                            if current == 0 {
                                continue;
                            }
                        }
                        Request::Attach(_) => {
                            if current == 0 {
                                assert!(
                                    write_server_message(
                                        &mut write_half,
                                        ServerMessage::Reply {
                                            id: envelope.id,
                                            reply: Reply::Attached(dummy_snapshot()),
                                        },
                                    )
                                    .await
                                );
                                return;
                            }
                            std::future::pending::<()>().await;
                        }
                        _ => return,
                    }
                }
            });
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    opts.request_timeout = Duration::from_millis(40);
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();

    let mut attempts = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), async {
        while attempts.len() < 3 {
            if let Some(ClientEvent::Reconnecting { attempt, .. }) = events.recv().await {
                attempts.push(attempt);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts, vec![1, 2, 3]);

    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_attach_uses_attempt_identity_for_aba() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("reconnect-aba.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let daemon_release = release.clone();
    let (attach_tx, mut attach_rx) = mpsc::unbounded_channel();
    let daemon_task = tokio::spawn(async move {
        let mut connection = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let current = connection;
            connection += 1;
            let release = daemon_release.clone();
            let attach_tx = attach_tx.clone();
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                let mut reconnect_attach_count = 0usize;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                    match envelope.req {
                        Request::Hello(_) => {
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Hello {
                                            daemon_version: "t".into(),
                                            protocol: PROTOCOL_VERSION,
                                            pid: 1,
                                        },
                                    },
                                )
                                .await
                            );
                        }
                        Request::Attach(attach) => {
                            if current == 1 {
                                let _ = attach_tx.send((envelope.id, attach));
                                reconnect_attach_count += 1;
                                if reconnect_attach_count == 1 {
                                    release.notified().await;
                                }
                            }
                            assert!(
                                write_server_message(
                                    &mut write_half,
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Attached(dummy_snapshot()),
                                    },
                                )
                                .await
                            );
                            if current == 0 {
                                return;
                            }
                        }
                        _ => return,
                    }
                }
            });
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let target = Attach::Resume {
        session: SessionId::new("ses_test"),
    };
    client.attach(target.clone()).await.unwrap();
    let (first_id, first_attach) = tokio::time::timeout(Duration::from_secs(5), attach_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_attach, target);
    assert!(matches!(
        client.attach(target.clone()).await,
        Err(ClientError::Disconnected)
    ));
    release.notify_one();
    let (second_id, second_attach) = tokio::time::timeout(Duration::from_secs(5), attach_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_attach, first_attach);
    assert_ne!(second_id, first_id);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(ClientEvent::Snapshot { context, .. }) = events.recv().await {
                assert_eq!(context.session, Some(SessionId::new("ses_test")));
                assert_eq!(context.session_epoch, 2);
                break;
            }
        }
    })
    .await
    .unwrap();

    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_daemon_ctl_helpers() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("missing.sock");

    // Missing socket
    assert!(!wait_for_socket(&socket, Duration::from_millis(30)).await);
    assert_eq!(daemon_status(&socket, "1.0").await, None);

    // Live daemon
    let live_socket = dir.path().join("live.sock");
    let listener = UnixListener::bind(&live_socket).unwrap();
    let cancel = CancellationToken::new();
    let daemon_task = tokio::spawn(run_fake_daemon(
        listener,
        cancel.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ));

    assert!(wait_for_socket(&live_socket, Duration::from_secs(1)).await);
    let status = daemon_status(&live_socket, "t").await;
    assert_eq!(
        status,
        Some(pacode_client::daemon_ctl::DaemonStatus {
            pid: 1,
            version: "t".into(),
            protocol: PROTOCOL_VERSION,
            socket: live_socket.clone(),
        })
    );

    stop_daemon(&live_socket, "t", false).await.unwrap();

    cancel.cancel();
    let _ = daemon_task.await;
}

#[test]
fn test_spawn_daemon() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let log_file = dir.path().join("daemon.log");

    // Spawn a dummy process (e.g. echo or true)
    let pid = spawn_daemon(
        std::path::Path::new("/bin/echo"),
        &socket,
        &log_file,
        &["--test".to_string()],
    )
    .unwrap();

    assert!(pid > 0);
    assert!(log_file.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_handshake_event_buffer_is_bounded() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("handshake-overflow.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        for seq in 1..=129 {
            if !write_server_message(
                &mut write_half,
                ServerMessage::Event {
                    seq,
                    event: Event::SessionStatus {
                        session: SessionId::new(format!("ses_{seq}")),
                        status: SessionStatusKind::Done,
                    },
                },
            )
            .await
            {
                return;
            }
        }
        let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
        let _ = write_server_message(
            &mut write_half,
            ServerMessage::Reply {
                id: hello.id,
                reply: Reply::Hello {
                    daemon_version: "t".into(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            },
        )
        .await;
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let result = tokio::time::timeout(Duration::from_secs(2), Client::connect(opts))
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(ClientError::UnexpectedReply { reply, .. })
            if reply.contains("event buffer overflow")
    ));
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn global_status_storm_is_coalesced_during_handshake() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("handshake-global-storm.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        for seq in 1..=1100 {
            assert!(
                write_server_message(
                    &mut write_half,
                    ServerMessage::Event {
                        seq,
                        event: Event::SessionStatus {
                            session: SessionId::new("ses_storm"),
                            status: SessionStatusKind::Done,
                        },
                    },
                )
                .await
            );
        }
        let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
        assert!(
            write_server_message(
                &mut write_half,
                ServerMessage::Reply {
                    id: hello.id,
                    reply: Reply::Hello {
                        daemon_version: "t".into(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
            )
            .await
        );
        std::future::pending::<()>().await;
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Event {
            seq: 1100,
            event: Event::SessionStatus {
                session,
                status: SessionStatusKind::Done,
            },
            ..
        }) if session == SessionId::new("ses_storm")
    ));
    assert!(events.try_recv().is_none());
    assert!(client.is_connected());

    client.close().await;
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn global_status_storm_is_coalesced_on_active_connection() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("active-global-storm.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    for seq in 1..=1100 {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::SessionStatus {
                                        session: SessionId::new("ses_test"),
                                        status: SessionStatusKind::Done,
                                    },
                                },
                            )
                            .await
                        );
                    }
                }
                Request::Ping => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Pong,
                            },
                        )
                        .await
                    );
                    std::future::pending::<()>().await;
                }
                _ => return,
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap(),
        Some(ClientEvent::Event {
            seq: 1100,
            event: Event::SessionStatus {
                status: SessionStatusKind::Done,
                ..
            },
            ..
        })
    ));
    assert!(events.try_recv().is_none());
    assert!(client.is_connected());

    client.close().await;
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn control_event_overflow_is_terminal() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("control-overflow.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    for seq in 1..=1100 {
                        if !write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq,
                                event: Event::Toast {
                                    level: ToastLevel::Info,
                                    title: seq.to_string(),
                                    detail: None,
                                },
                            },
                        )
                        .await
                        {
                            return;
                        }
                    }
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Envelope = serde_json::from_str(line.trim()).unwrap();
                    assert!(matches!(request.req, Request::Ping));
                    let _ = write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: request.id,
                            reply: Reply::Pong,
                        },
                    )
                    .await;
                    return;
                }
                _ => return,
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), client.request(Request::Ping))
            .await
            .unwrap(),
        Err(ClientError::Disconnected | ClientError::Timeout(_))
    ));

    let mut controls = 0;
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Event {
                    event: Event::Toast { .. },
                    ..
                }) => controls += 1,
                Some(ClientEvent::Disconnected { reason, .. })
                    if reason.contains("event queue overflow") =>
                {
                    break reason;
                }
                Some(_) => {}
                None => panic!("event stream closed without overflow notification"),
            }
        }
    })
    .await
    .unwrap();
    assert!(controls > 0);
    assert!(terminal.contains("event queue overflow"));
    assert!(!client.is_connected());
    client.close().await;
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_protocol_mismatch_is_terminal() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("reconnect-mismatch.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon_task = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let first_task = tokio::spawn(async move {
            let (read_half, mut write_half) = first_stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
            assert!(
                write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: hello.id,
                        reply: Reply::Hello {
                            daemon_version: "t".into(),
                            protocol: PROTOCOL_VERSION,
                            pid: 1,
                        },
                    },
                )
                .await
            );
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let attach: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
            assert!(
                write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: attach.envelope.id,
                        reply: Reply::Attached(dummy_snapshot()),
                    },
                )
                .await
            );
        });
        let (second_stream, _) = listener.accept().await.unwrap();
        let second_task = tokio::spawn(async move {
            let (read_half, mut write_half) = second_stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
            assert!(
                write_server_message(
                    &mut write_half,
                    ServerMessage::Reply {
                        id: hello.id,
                        reply: Reply::Hello {
                            daemon_version: "old".into(),
                            protocol: PROTOCOL_VERSION - 1,
                            pid: 2,
                        },
                    },
                )
                .await
            );
            let _ = reader.read_line(&mut line).await;
        });
        let _ = first_task.await;
        let _ = second_task.await;
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let _ = client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await;

    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Disconnected { reason, .. })
                    if reason.contains("protocol mismatch") =>
                {
                    break reason;
                }
                Some(_) => {}
                None => panic!("event stream closed before protocol mismatch"),
            }
        }
    })
    .await
    .unwrap();
    // Both numbers derived: the scripted daemon above answers with
    // `PROTOCOL_VERSION - 1`, and a version spelled out here would fail on
    // every protocol bump for a reason that has nothing to do with
    // reconnection.
    assert!(
        terminal.contains(&format!("daemon speaks {}", PROTOCOL_VERSION - 1)),
        "{terminal}"
    );
    assert!(
        terminal.contains(&format!("client {PROTOCOL_VERSION}")),
        "{terminal}"
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await,
        Err(_) | Ok(None)
    ));
    assert!(!client.is_connected());

    client.close().await;
    daemon_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_protocol_mismatch() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("mismatch.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            let reply = ServerMessage::Reply {
                id: 1,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: 999,
                    pid: 1,
                },
            };
            let mut out = serde_json::to_string(&reply).unwrap();
            out.push('\n');
            let _ = write_half.write_all(out.as_bytes()).await;
            let _ = write_half.flush().await;
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let res = Client::connect(opts).await;
    match res {
        Err(ClientError::Protocol { daemon, client }) => {
            assert_eq!(daemon, 999);
            assert_eq!(client, PROTOCOL_VERSION);
        }
        other => panic!("expected ClientError::Protocol, got {other:?}"),
    }

    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_and_attach_daemon_error() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("err.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();

            // Hello
            let _ = reader.read_line(&mut line).await;
            let reply = ServerMessage::Reply {
                id: 1,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            };
            let mut out = serde_json::to_string(&reply).unwrap();
            out.push('\n');
            let _ = write_half.write_all(out.as_bytes()).await;

            // Next request (Attach)
            line.clear();
            let _ = reader.read_line(&mut line).await;
            let env: Envelope = serde_json::from_str(line.trim()).unwrap();
            let reply = ServerMessage::Reply {
                id: env.id,
                reply: Reply::Error {
                    message: "session not found".to_string(),
                },
            };
            let mut out = serde_json::to_string(&reply).unwrap();
            out.push('\n');
            let _ = write_half.write_all(out.as_bytes()).await;

            // Next request (ok)
            line.clear();
            let _ = reader.read_line(&mut line).await;
            let env: Envelope = serde_json::from_str(line.trim()).unwrap();
            let reply = ServerMessage::Reply {
                id: env.id,
                reply: Reply::Error {
                    message: "permission denied".to_string(),
                },
            };
            let mut out = serde_json::to_string(&reply).unwrap();
            out.push('\n');
            let _ = write_half.write_all(out.as_bytes()).await;
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, _rx) = Client::connect(opts).await.unwrap();

    // Attach returns Daemon error
    let attach_res = client
        .attach(Attach::Resume {
            session: SessionId::new("missing"),
        })
        .await;
    match attach_res {
        Err(ClientError::Daemon(msg)) => assert_eq!(msg, "session not found"),
        other => panic!("expected ClientError::Daemon, got {other:?}"),
    }
    assert!(matches!(
        client
            .retry_attach(Attach::Resume {
                session: SessionId::new("missing"),
            })
            .await,
        Err(ClientError::StaleSession)
    ));

    // ok() returns Daemon error
    let ok_res = client.ok(Request::Ping).await;
    match ok_res {
        Err(ClientError::Daemon(msg)) => assert_eq!(msg, "permission denied"),
        other => panic!("expected ClientError::Daemon, got {other:?}"),
    }

    client.close().await;
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lagged_resync_marker_becomes_client_event_with_current_context() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("resync-marker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".to_string(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Resync {
                                current_seq: Some(1),
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1,
                                event: Event::Toast {
                                    level: ToastLevel::Info,
                                    title: "after-resync".into(),
                                    detail: None,
                                },
                            },
                        )
                        .await
                    );
                    return;
                }
                _ => return,
            }
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let context = client.event_context();

    let marker = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        marker,
        ClientEvent::ResyncNeeded {
            context: actual
        } if actual == context
    ));
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        ClientEvent::Event {
            context: actual,
            seq: 1,
            event: Event::Toast { .. },
        } if actual == context
    ));

    client.close().await;
    server_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn event_stream_delivers_every_event_in_order() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("lossless-events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    for seq in 1..=1100 {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::TextDelta {
                                        agent: AgentId::main(),
                                        item_seq: seq,
                                        text: format!("delta_{seq}"),
                                    },
                                },
                            )
                            .await
                        );
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1101,
                                event: Event::TurnEnded {
                                    agent: AgentId::main(),
                                    turn: TurnId::new("trn_test"),
                                    usage: None,
                                    stop: TurnStop::Completed,
                                },
                            },
                        )
                        .await
                    );
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                    assert!(matches!(envelope.req, Request::Ping));
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Pong,
                            },
                        )
                        .await
                    );
                    return;
                }
                _ => return,
            }
        }
    });
    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);
    server_task.await.unwrap();

    let mut seqs = Vec::new();
    let mut resyncs = 0;
    let mut saw_terminal = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::Event {
                    seq,
                    event: Event::TextDelta { .. },
                    ..
                } => seqs.push(seq),
                ClientEvent::Event {
                    seq: 1101,
                    event: Event::TurnEnded { .. },
                    ..
                } => {
                    saw_terminal = true;
                    break;
                }
                ClientEvent::ResyncNeeded { .. } => resyncs += 1,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_terminal);
    assert!(seqs.len() <= 1022);
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(resyncs, 1);

    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_reply_reopens_delta_stream_after_resync() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("resync-recovery.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    for seq in 1..=1100 {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::TextDelta {
                                        agent: AgentId::main(),
                                        item_seq: seq,
                                        text: format!("delta_{seq}"),
                                    },
                                },
                            )
                            .await
                        );
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Resync {
                                current_seq: Some(1100),
                            },
                        )
                        .await
                    );
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Envelope = serde_json::from_str(line.trim()).unwrap();
                    if matches!(request.req, Request::Ping) {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Reply {
                                    id: request.id,
                                    reply: Reply::Pong,
                                },
                            )
                            .await
                        );
                        line.clear();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                    }
                    let request: Envelope = serde_json::from_str(line.trim()).unwrap();
                    assert!(matches!(request.req, Request::GetSnapshot));
                    let mut snapshot = dummy_snapshot();
                    snapshot.seq = 1100;
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: request.id,
                                reply: Reply::Snapshot(snapshot),
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1101,
                                event: Event::TextDelta {
                                    agent: AgentId::main(),
                                    item_seq: 1101,
                                    text: "after_snapshot".into(),
                                },
                            },
                        )
                        .await
                    );
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Envelope = serde_json::from_str(line.trim()).unwrap();
                    assert!(matches!(request.req, Request::Ping));
                    let _ = write_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id: request.id,
                            reply: Reply::Pong,
                        },
                    )
                    .await;
                    return;
                }
                _ => return,
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(events.recv().await, Some(ClientEvent::ResyncNeeded { .. })) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let context = client.event_context();
    assert!(matches!(
        client
            .request_session_for(context, Request::GetSnapshot)
            .await
            .unwrap(),
        Reply::Snapshot(snapshot) if snapshot.seq == 1100
    ));
    let mut saw_after_snapshot = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Event {
                    seq: 1101,
                    event: Event::TextDelta { .. },
                    ..
                }) => {
                    saw_after_snapshot = true;
                    break;
                }
                Some(_) => {}
                None => panic!("event stream closed before post-snapshot delta"),
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_after_snapshot);
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);

    client.close().await;
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn full_delta_queue_does_not_block_permission_reply() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("permission-reply-queue.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let permission_id = PermissionId::new("perm_test");
    let permission = PermissionRequest {
        id: permission_id.clone(),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: CallId::new("call_test"),
        tool: "Bash".into(),
        title: "run tests".into(),
        detail: "cargo test -p pacode-client".into(),
        risk: Some(RiskLevel::Confirm),
        created_at_ms: 1000,
    };
    let (flood_tx, mut flood_rx) = mpsc::unbounded_channel();
    let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
    let server_permission_id = permission_id.clone();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1,
                                event: Event::PermissionRequested(permission),
                            },
                        )
                        .await
                    );
                    flood_rx.recv().await.unwrap();
                    for seq in 2..=1101 {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::TextDelta {
                                        agent: AgentId::main(),
                                        item_seq: seq,
                                        text: format!("delta_{seq}"),
                                    },
                                },
                            )
                            .await
                        );
                    }
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1102,
                                event: Event::PermissionExplained {
                                    permission: server_permission_id.clone(),
                                    text: "Runs the client test suite.".into(),
                                    level: None,
                                },
                            },
                        )
                        .await
                    );
                    terminal_tx.send(()).unwrap();
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                    let Request::PermissionReply {
                        permission,
                        decision,
                        reason,
                    } = envelope.req
                    else {
                        return;
                    };
                    assert_eq!(permission, server_permission_id);
                    assert_eq!(decision, PermissionDecision::AllowOnce);
                    assert_eq!(reason, None);
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1103,
                                event: Event::PermissionResolved {
                                    permission: server_permission_id,
                                    decision: PermissionDecision::AllowOnce,
                                    reason: None,
                                },
                            },
                        )
                        .await
                    );
                    return;
                }
                _ => return,
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    let requested_permission = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ClientEvent::Event {
                event: Event::PermissionRequested(permission),
                ..
            } = events.recv().await.unwrap()
            {
                break permission;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(requested_permission.id, permission_id);
    flood_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), terminal_rx.recv())
        .await
        .unwrap()
        .unwrap();

    tokio::time::timeout(
        Duration::from_secs(5),
        client.ok(Request::PermissionReply {
            permission: permission_id,
            decision: PermissionDecision::AllowOnce,
            reason: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();

    let mut delta_seqs = Vec::new();
    let mut resyncs = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                ClientEvent::Event {
                    seq,
                    event: Event::TextDelta { .. },
                    ..
                } => delta_seqs.push(seq),
                ClientEvent::ResyncNeeded { .. } => resyncs += 1,
                ClientEvent::Event {
                    seq: 1102,
                    event: Event::PermissionExplained { .. },
                    ..
                } => {}
                ClientEvent::Event {
                    seq: 1103,
                    event: Event::PermissionResolved { .. },
                    ..
                } => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(delta_seqs.len() <= 1022);
    assert!(delta_seqs.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(resyncs, 1);
    server_task.await.unwrap();

    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_event_floor_tracks_only_session_events_and_snapshot() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("session-event-floor.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut snapshot_count = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".to_string(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                }
                Request::UserMessage { .. } => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 5,
                                event: Event::Toast {
                                    level: ToastLevel::Info,
                                    title: "session".into(),
                                    detail: None,
                                },
                            },
                        )
                        .await
                    );
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 100,
                                event: Event::SessionStatus {
                                    session: SessionId::new("ses_test"),
                                    status: SessionStatusKind::Working,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::GetSnapshot => {
                    snapshot_count += 1;
                    let mut snapshot = dummy_snapshot();
                    snapshot.seq = match snapshot_count {
                        1 => 10,
                        2 => 5,
                        _ => 20,
                    };
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Snapshot(snapshot),
                            },
                        )
                        .await
                    );
                }
                _ => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                }
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    assert_eq!(client.session_event_floor(), 0);

    client
        .request(Request::UserMessage {
            text: "hello".into(),
            images: Vec::new(),
            message_id: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    let mut event_seqs = Vec::new();
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            ClientEvent::Event { seq, .. } => event_seqs.push(seq),
            event => panic!("unexpected event: {event:?}"),
        }
    }
    assert_eq!(event_seqs, [5, 100]);
    assert_eq!(client.session_event_floor(), 5);

    let reply = client.request(Request::GetSnapshot).await.unwrap();
    assert!(matches!(reply, Reply::Snapshot(snapshot) if snapshot.seq == 10));
    assert_eq!(client.session_event_floor(), 10);

    let stale = client.request(Request::GetSnapshot).await.unwrap();
    assert!(matches!(stale, Reply::Snapshot(snapshot) if snapshot.seq == 5));
    assert_eq!(client.session_event_floor(), 10);

    let current = client.request(Request::GetSnapshot).await.unwrap();
    assert!(matches!(current, Reply::Snapshot(snapshot) if snapshot.seq == 20));
    assert_eq!(client.session_event_floor(), 20);

    client.close().await;
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_mcp_and_plugin_helpers() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("mcp_plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();

            // Hello
            let _ = reader.read_line(&mut line).await;
            let hello_reply = ServerMessage::Reply {
                id: 1,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            };
            let mut out = serde_json::to_string(&hello_reply).unwrap();
            out.push('\n');
            let _ = write_half.write_all(out.as_bytes()).await;

            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let env: Envelope = serde_json::from_str(trimmed).unwrap();
                let reply = match env.req {
                    Request::ListMcpServers => Reply::McpServers {
                        servers: vec![pacode_types::McpServerInfo {
                            name: "github".into(),
                            status: "ready".into(),
                            error: None,
                            tools: 5,
                            resources: 2,
                            prompts: 1,
                            prompt_names: vec!["review".into()],
                        }],
                    },
                    Request::RestartMcpServer { server } => {
                        assert_eq!(server, "github");
                        Reply::Ok
                    }
                    Request::SetMcpServerEnabled { server, enabled } => {
                        assert_eq!(server, "github");
                        assert!(!enabled);
                        Reply::Ok
                    }
                    Request::GetMcpPrompt { server, name, args } => {
                        assert_eq!(server, "github");
                        assert_eq!(name, "review");
                        assert_eq!(args.get("pr"), Some(&"123".to_string()));
                        Reply::McpPrompt {
                            text: "Review prompt content".into(),
                        }
                    }
                    Request::ListPlugins => Reply::Plugins {
                        plugins: vec![pacode_types::PluginInfo {
                            name: "test-plugin".into(),
                            version: "0.1.0".into(),
                            kind: "lua".into(),
                            tools: vec!["tool1".into()],
                            commands: vec!["hello".into()],
                            error: None,
                            loaded: true,
                            ..pacode_types::PluginInfo::default()
                        }],
                    },
                    Request::RunPluginCommand { name, args } => {
                        assert_eq!(name, "hello");
                        assert_eq!(args, "world");
                        Reply::PluginCommand(pacode_types::PluginCommandOutcome::InsertText {
                            text: "Hello, world!".into(),
                        })
                    }
                    Request::ListSessions { limit } => {
                        assert_eq!(limit, 10);
                        Reply::Sessions {
                            sessions: vec![dummy_snapshot().meta],
                        }
                    }
                    Request::ListModels => Reply::Models {
                        models: vec![pacode_types::model::ModelInfo {
                            route: ModelRoute::new("dummy", "model"),
                            display_name: "Dummy Model".into(),
                            context_window: Some(32000),
                            supports_reasoning: false,
                            supports_vision: false,
                            supported_efforts: None,
                            pricing: None,
                            ..Default::default()
                        }],
                    },
                    _ => Reply::Ok,
                };

                let msg = ServerMessage::Reply { id: env.id, reply };
                let mut out = serde_json::to_string(&msg).unwrap();
                out.push('\n');
                let _ = write_half.write_all(out.as_bytes()).await;
                let _ = write_half.flush().await;
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, _rx) = Client::connect(opts).await.unwrap();

    // 1. list_mcp_servers
    let servers = client.list_mcp_servers().await.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "github");
    assert_eq!(servers[0].prompt_names, vec!["review"]);

    // 2. restart_mcp_server
    client.restart_mcp_server("github").await.unwrap();

    // 3. set_mcp_server_enabled
    client
        .set_mcp_server_enabled("github", false)
        .await
        .unwrap();

    // 4. get_mcp_prompt
    let mut args = std::collections::BTreeMap::new();
    args.insert("pr".to_string(), "123".to_string());
    let prompt_text = client
        .get_mcp_prompt("github", "review", args)
        .await
        .unwrap();
    assert_eq!(prompt_text, "Review prompt content");

    // 5. list_plugins
    let plugins = client.list_plugins().await.unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].name, "test-plugin");
    assert_eq!(plugins[0].commands, vec!["hello"]);

    // 6. run_plugin_command
    let outcome = client.run_plugin_command("hello", "world").await.unwrap();
    assert_eq!(
        outcome,
        pacode_types::PluginCommandOutcome::InsertText {
            text: "Hello, world!".into(),
        }
    );

    // 7. list_sessions
    let sessions = client.list_sessions(10).await.unwrap();
    assert_eq!(sessions.len(), 1);

    // 8. list_models
    let models = client.list_models().await.unwrap();
    assert_eq!(models.len(), 1);

    client.close().await;
    let _ = server_task.await;
}

#[test]
fn test_mcp_and_plugin_wire_serialization_round_trip() {
    // 1. ListMcpServers
    let env = Envelope {
        id: 10,
        req: Request::ListMcpServers,
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // 2. RestartMcpServer
    let env = Envelope {
        id: 11,
        req: Request::RestartMcpServer {
            server: "server1".into(),
        },
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // 3. SetMcpServerEnabled
    let env = Envelope {
        id: 12,
        req: Request::SetMcpServerEnabled {
            server: "server1".into(),
            enabled: true,
        },
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // 4. GetMcpPrompt
    let mut args = std::collections::BTreeMap::new();
    args.insert("key".into(), "val".into());
    let env = Envelope {
        id: 13,
        req: Request::GetMcpPrompt {
            server: "server1".into(),
            name: "prompt1".into(),
            args,
        },
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // 5. ListPlugins
    let env = Envelope {
        id: 14,
        req: Request::ListPlugins,
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // 6. RunPluginCommand
    let env = Envelope {
        id: 15,
        req: Request::RunPluginCommand {
            name: "cmd1".into(),
            args: "arg1 arg2".into(),
        },
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, env);

    // Replies
    let reply_mcp = ServerMessage::Reply {
        id: 10,
        reply: Reply::McpServers {
            servers: vec![pacode_types::McpServerInfo {
                name: "srv".into(),
                status: "ready".into(),
                error: Some("err".into()),
                tools: 3,
                resources: 1,
                prompts: 2,
                prompt_names: vec!["p1".into()],
            }],
        },
    };
    let json = serde_json::to_string(&reply_mcp).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, reply_mcp);

    let reply_prompt = ServerMessage::Reply {
        id: 13,
        reply: Reply::McpPrompt {
            text: "rendered prompt".into(),
        },
    };
    let json = serde_json::to_string(&reply_prompt).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, reply_prompt);

    let reply_plugins = ServerMessage::Reply {
        id: 14,
        reply: Reply::Plugins {
            plugins: vec![pacode_types::PluginInfo {
                name: "p".into(),
                version: "1.0".into(),
                kind: "wasm".into(),
                tools: vec![],
                commands: vec!["run".into()],
                error: None,
                loaded: true,
                ..pacode_types::PluginInfo::default()
            }],
        },
    };
    let json = serde_json::to_string(&reply_plugins).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, reply_plugins);

    let reply_cmd = ServerMessage::Reply {
        id: 15,
        reply: Reply::PluginCommand(pacode_types::PluginCommandOutcome::SendPrompt {
            text: "do this".into(),
        }),
    };
    let json = serde_json::to_string(&reply_cmd).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, reply_cmd);
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_rejects_oversized_ndjson_line() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("handshake-line-overflow.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap_or(0) > 0);
        let _hello: Envelope = serde_json::from_str(line.trim()).unwrap();
        let mut payload = vec![b'x'; OVERSIZED_NDJSON_LINE];
        payload.push(b'\n');
        let _ = write_half.write_all(&payload).await;
        let _ = write_half.flush().await;
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let result = tokio::time::timeout(Duration::from_secs(2), Client::connect(opts))
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(ClientError::UnexpectedReply { reply, .. })
            if reply.contains("NDJSON line too long")
    ));
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn steady_reader_rejects_oversized_ndjson_line_without_retry() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("reader-line-overflow.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap_or(0) > 0);
        let hello: Envelope = serde_json::from_str(line.trim()).unwrap();
        assert!(
            write_server_message(
                &mut write_half,
                ServerMessage::Reply {
                    id: hello.id,
                    reply: Reply::Hello {
                        daemon_version: "t".into(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
            )
            .await
        );
        let mut payload = vec![b'x'; OVERSIZED_NDJSON_LINE];
        payload.push(b'\n');
        let _ = write_half.write_all(&payload).await;
        let _ = write_half.flush().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));

    let reason = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Disconnected { reason, .. })
                    if reason.contains("NDJSON line too long") =>
                {
                    break reason;
                }
                Some(ClientEvent::Reconnecting { .. }) => panic!("line overflow must not retry"),
                Some(_) => {}
                None => panic!("event stream closed before line overflow"),
            }
        }
    })
    .await
    .unwrap();
    assert!(reason.contains("NDJSON line too long"));
    assert!(!client.is_connected());
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await,
        Ok(None)
    ));
    assert!(matches!(
        client.request(Request::Ping).await,
        Err(ClientError::Disconnected)
    ));

    client.close().await;
    server_task.abort();
}

fn w128_agent_info(index: usize) -> AgentInfo {
    AgentInfo {
        id: AgentId::new(format!("agt_w128_{index}")),
        name: format!("agent-{index}"),
        kind: AgentKind::Sub,
        status: AgentStatus::Finished,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: Some(2),
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn w128_control_events_are_retained_while_deltas_coalesce() {
    const W128: usize = 128;
    const DELTAS: usize = 1_100;

    let dir = tempdir().unwrap();
    let socket = dir.path().join("w128-coalesce.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let sent = Arc::new(Barrier::new(2));
    let release = Arc::new(Notify::new());
    let (burst_processed_tx, burst_processed_rx) = tokio::sync::oneshot::channel();
    let server_sent = sent.clone();
    let server_release = release.clone();
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            match envelope.req {
                Request::Hello(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Hello {
                                    daemon_version: "t".into(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await
                    );
                }
                Request::Attach(_) => {
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Attached(dummy_snapshot()),
                            },
                        )
                        .await
                    );
                    let mut seq = 1u64;
                    for index in 0..W128 {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::AgentAdded(w128_agent_info(index)),
                                },
                            )
                            .await
                        );
                        seq += 1;
                    }
                    for _ in 0..DELTAS {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Event {
                                    seq,
                                    event: Event::TextDelta {
                                        agent: AgentId::main(),
                                        item_seq: seq,
                                        text: "d".into(),
                                    },
                                },
                            )
                            .await
                        );
                        seq += 1;
                    }
                    server_sent.wait().await;
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Envelope = serde_json::from_str(line.trim()).unwrap();
                    assert!(matches!(request.req, Request::Ping));
                    assert!(
                        write_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: request.id,
                                reply: Reply::Pong,
                            },
                        )
                        .await
                    );
                    let _ = burst_processed_tx.send(());
                    server_release.notified().await;
                    return;
                }
                _ => return,
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), sent.wait())
        .await
        .unwrap();
    assert_eq!(client.request(Request::Ping).await.unwrap(), Reply::Pong);
    burst_processed_rx.await.unwrap();

    let expected_context = client.event_context();
    let mut agent_ids = Vec::with_capacity(W128);
    let mut delta_count = 0;
    let mut last_seq = 0;
    let mut resync_count = 0;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            ClientEvent::Event {
                context,
                event: Event::AgentAdded(info),
                ..
            } => {
                assert_eq!(context, expected_context);
                agent_ids.push(info.id);
            }
            ClientEvent::Event {
                context,
                seq,
                event: Event::TextDelta { .. },
            } => {
                assert_eq!(context, expected_context);
                assert!(seq > last_seq);
                last_seq = seq;
                delta_count += 1;
            }
            ClientEvent::ResyncNeeded { context } => {
                assert_eq!(context, expected_context);
                resync_count += 1;
                break;
            }
            ClientEvent::Disconnected { .. } => {
                panic!("event stream closed before coalescing")
            }
            _ => {}
        }
    }

    assert_eq!(agent_ids.len(), W128);
    assert_eq!(
        agent_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        W128
    );
    assert_eq!(delta_count, 894);
    assert_eq!(resync_count, 1);

    release.notify_one();
    client.close().await;
    server_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn w128_reconnect_orders_recovery_events() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("w128-reconnect.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let first_release = Arc::new(Barrier::new(2));
    let second_release = Arc::new(Notify::new());
    let server_first_release = first_release.clone();
    let server_second_release = second_release.clone();
    let server_task = tokio::spawn(async move {
        for connection in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
                match envelope.req {
                    Request::Hello(_) => {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Reply {
                                    id: envelope.id,
                                    reply: Reply::Hello {
                                        daemon_version: "t".into(),
                                        protocol: PROTOCOL_VERSION,
                                        pid: 1,
                                    },
                                },
                            )
                            .await
                        );
                    }
                    Request::Attach(_) => {
                        assert!(
                            write_server_message(
                                &mut write_half,
                                ServerMessage::Reply {
                                    id: envelope.id,
                                    reply: Reply::Attached(dummy_snapshot()),
                                },
                            )
                            .await
                        );
                        if connection == 0 {
                            server_first_release.wait().await;
                        } else {
                            server_second_release.notified().await;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    client
        .attach(Attach::Resume {
            session: SessionId::new("ses_test"),
        })
        .await
        .unwrap();
    first_release.wait().await;

    let mut order = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            ClientEvent::Disconnected { .. } => {
                assert!(order.is_empty());
                order.push("disconnected");
            }
            ClientEvent::Reconnecting { .. } => {
                assert_eq!(order.last(), Some(&"disconnected"));
                order.push("reconnecting");
            }
            ClientEvent::Connected { context, .. } if context.connection_epoch == 2 => {
                assert_eq!(order.last(), Some(&"reconnecting"));
                order.push("connected");
            }
            ClientEvent::Snapshot { context, .. } if context.connection_epoch == 2 => {
                assert_eq!(order.last(), Some(&"connected"));
                order.push("snapshot");
                break;
            }
            _ => {}
        }
    }

    assert_eq!(
        order,
        vec!["disconnected", "reconnecting", "connected", "snapshot"]
    );
    second_release.notify_one();
    client.close().await;
    server_task.await.unwrap();
}
