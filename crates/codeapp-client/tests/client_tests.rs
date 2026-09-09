use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codeapp_client::daemon_ctl::{daemon_status, spawn_daemon, stop_daemon, wait_for_socket};
use codeapp_client::{Client, ClientError, ClientEvent, ClientOptions};
use codeapp_config::Paths;
use codeapp_types::ids::SessionId;
use codeapp_types::model::{Effort, ModelRoute};
use codeapp_types::state::{Mode, Plan, SessionMeta, ToastLevel, UsageTotals};
use codeapp_types::{
    Attach, Envelope, Event, PROTOCOL_VERSION, Reply, Request, ServerMessage, SessionSnapshot,
};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

fn dummy_snapshot() -> SessionSnapshot {
    SessionSnapshot {
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
        },
        agents: vec![],
        plan: Plan::default(),
        tasks: vec![],
        usage: UsageTotals::default(),
        transcript: vec![],
        has_more_history: false,
        pending_permissions: vec![],
        turn_active: false,
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
            daemon_version: "t".into(),
            pid: 1,
            version_mismatch: false,
        }
    );

    client.close().await;
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
        })
        .await
        .unwrap();

    let ev1 = rx.recv().await.unwrap();
    assert_eq!(
        ev1,
        ClientEvent::Event {
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
    assert!(matches!(res, Err(ClientError::Timeout)));

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
    assert_eq!(reconn, ClientEvent::Reconnecting { attempt: 1 });

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
    let ev_conn = rx.recv().await.unwrap();
    assert!(matches!(ev_conn, ClientEvent::Connected { .. }));

    let ev_snap = rx.recv().await.unwrap();
    assert!(matches!(ev_snap, ClientEvent::Snapshot(_)));

    assert!(client.is_connected());
    let pong = client.request(Request::Ping).await.unwrap();
    assert_eq!(pong, Reply::Pong);

    client.close().await;
    cancel2.cancel();
    let _ = daemon_task2.await;
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
        Some(codeapp_client::daemon_ctl::DaemonStatus {
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

    // ok() returns Daemon error
    let ok_res = client.ok(Request::Interrupt).await;
    match ok_res {
        Err(ClientError::Daemon(msg)) => assert_eq!(msg, "permission denied"),
        other => panic!("expected ClientError::Daemon, got {other:?}"),
    }

    client.close().await;
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_event_queue_drop_on_lagging_receiver() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("drop.sock");
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

            // Send 1100 events rapidly without waiting for client to read
            for seq in 1..=1100 {
                let ev = ServerMessage::Event {
                    seq,
                    event: Event::Toast {
                        level: ToastLevel::Info,
                        title: format!("ev_{seq}"),
                        detail: None,
                    },
                };
                let mut out = serde_json::to_string(&ev).unwrap();
                out.push('\n');
                if write_half.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
            let _ = write_half.flush().await;

            // Keep connection open briefly
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let mut opts = ClientOptions::new(Paths::under(dir.path()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;

    let (client, mut rx) = Client::connect(opts).await.unwrap();

    // Wait for server to finish sending and exit
    let _ = server_task.await;
    // Give brief time for reader to finish processing EOF
    tokio::time::sleep(Duration::from_millis(50)).await;

    // First event is Connected
    let first = rx.recv().await.unwrap();
    assert!(matches!(first, ClientEvent::Connected { .. }));

    // Read buffered events: up to capacity 1024 (1 Connected was already read)
    let mut count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
        count += 1;
    }

    // Since channel capacity is 1024 and receiver was not reading,
    // exactly 1023 events could fit (1024 capacity - 1 Connected).
    assert_eq!(count, 1023);

    client.close().await;
}
