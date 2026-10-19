use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_client_protocol::schema::v1::{
    ContentBlock, LoadSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent,
};
use pacode_config::Paths;
use pacode_types::ids::{AgentId, CallId, PermissionId, TurnId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{
    AgentInfo, AgentKind, AgentStatus, PermissionDecision, Plan, RiskLevel, SessionMeta,
    UsageTotals,
};
use pacode_types::{
    AttachEnvelope, Envelope, PROTOCOL_VERSION, ServerMessage, SessionSnapshot, SessionStatusKind,
    ToolStatus, TranscriptItem, TranscriptKind, TurnStop,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use super::*;

fn ids(names: &[&str]) -> VecDeque<PacodeSessionId> {
    names.iter().map(|n| PacodeSessionId::new(*n)).collect()
}

fn names(order: &VecDeque<PacodeSessionId>) -> Vec<String> {
    order.iter().map(|id| id.to_string()).collect()
}

fn event_context(connection_epoch: u64) -> ClientEventContext {
    ClientEventContext {
        connection_epoch,
        session_epoch: 4,
        session: Some(PacodeSessionId::new("ses_test")),
    }
}

fn connected(connection_epoch: u64) -> ClientEvent {
    ClientEvent::Connected {
        context: event_context(connection_epoch),
        daemon_version: "test".into(),
        pid: 1,
        version_mismatch: false,
    }
}

fn snapshot(connection_epoch: u64, seq: u64) -> ClientEvent {
    ClientEvent::Snapshot {
        context: event_context(connection_epoch),
        snapshot: SessionSnapshot {
            meta: SessionMeta {
                id: PacodeSessionId::new("ses_test"),
                name: None,
                cwd: PathBuf::from("/tmp"),
                git_branch: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                model: ModelRoute::new("provider", "model"),
                effort: Effort::Low,
                mode: Mode::Build,
                first_prompt: None,
                zdr: false,
            },
            agents: Vec::new(),
            plan: Plan::default(),
            tasks: Vec::new(),
            usage: UsageTotals::default(),
            transcript: Vec::new(),
            has_more_history: false,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
            turn_active: false,
            active_turn: None,
            terminal_turn: None,
            terminal_error: None,
            cron_jobs: Vec::new(),
            monitors: Vec::new(),
            seq,
        },
    }
}

fn turn_ended(connection_epoch: u64, seq: u64) -> ClientEvent {
    turn_ended_for(
        connection_epoch,
        seq,
        AgentId::main(),
        TurnId::new("turn_test"),
    )
}

fn turn_ended_for(connection_epoch: u64, seq: u64, agent: AgentId, turn: TurnId) -> ClientEvent {
    ClientEvent::Event {
        context: event_context(connection_epoch),
        seq,
        event: Event::TurnEnded {
            agent,
            turn,
            usage: None,
            stop: TurnStop::Completed,
        },
    }
}

fn admitted_turn(message_id: &str) -> TurnId {
    TurnId::new(format!("trn_{message_id}"))
}

fn turn_started(connection_epoch: u64, seq: u64, turn: &str) -> ClientEvent {
    ClientEvent::Event {
        context: event_context(connection_epoch),
        seq,
        event: Event::TurnStarted {
            agent: AgentId::main(),
            turn: TurnId::new(turn),
        },
    }
}

fn global_event(connection_epoch: u64, seq: u64) -> ClientEvent {
    ClientEvent::Event {
        context: event_context(connection_epoch),
        seq,
        event: Event::SessionStatus {
            session: PacodeSessionId::new("ses_test"),
            status: SessionStatusKind::Working,
        },
    }
}

fn unscoped_global_event(connection_epoch: u64, seq: u64) -> ClientEvent {
    ClientEvent::Event {
        context: ClientEventContext {
            connection_epoch,
            session_epoch: 0,
            session: None,
        },
        seq,
        event: Event::SessionStatus {
            session: PacodeSessionId::new("ses_other"),
            status: SessionStatusKind::Working,
        },
    }
}

fn snapshot_with_permissions(
    connection_epoch: u64,
    seq: u64,
    pending_permissions: Vec<PermissionRequest>,
) -> ClientEvent {
    let mut event = snapshot(connection_epoch, seq);
    let ClientEvent::Snapshot { snapshot, .. } = &mut event else {
        unreachable!()
    };
    snapshot.pending_permissions = pending_permissions;
    snapshot.turn_active = true;
    event
}

fn permission_request(id: &str) -> PermissionRequest {
    PermissionRequest {
        id: PermissionId::new(id),
        agent: AgentId::main(),
        agent_name: "main".to_string(),
        call_id: CallId::new("call_test"),
        tool: "bash".to_string(),
        title: "Run test command".to_string(),
        detail: "cargo test".to_string(),
        risk: Some(RiskLevel::Confirm),
        created_at_ms: 1,
    }
}

fn test_socket(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "pacode-acp-{name}-{}-{stamp}.sock",
        std::process::id()
    ))
}

async fn write_test_server_message(
    write_half: &mut OwnedWriteHalf,
    message: ServerMessage,
) -> bool {
    let mut line = serde_json::to_string(&message).unwrap();
    line.push('\n');
    write_half.write_all(line.as_bytes()).await.is_ok() && write_half.flush().await.is_ok()
}

async fn test_session_state(name: &str) -> (Arc<SessionState>, JoinHandle<()>) {
    let id = PacodeSessionId::new(format!("ses_{name}"));
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let socket = test_socket(name);
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let server_snapshot = initial.clone();
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
            let message = match envelope.req {
                Request::Hello(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
                Request::Attach(attach) => {
                    let mut snapshot = server_snapshot.clone();
                    if let Attach::Resume { session } = attach {
                        snapshot.meta.id = session;
                    }
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Attached(snapshot),
                    }
                }
                _ => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
            };
            assert!(write_test_server_message(&mut write_half, message).await);
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    (state, server)
}

async fn serve_close_connection(
    stream: UnixStream,
    snapshot: SessionSnapshot,
    fail_close: bool,
    request_order: Arc<Mutex<Vec<String>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        request_order
            .lock()
            .unwrap()
            .push(envelope.req.name().to_string());
        let is_detach = matches!(&envelope.req, Request::Detach);
        let reply = match envelope.req {
            Request::Hello(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            },
            Request::Attach(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(snapshot.clone()),
            },
            Request::CloseSession if fail_close => return,
            Request::CloseSession | Request::Detach => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            },
            _ => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            },
        };
        if !write_test_server_message(&mut write_half, reply).await {
            return;
        }
        if is_detach {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn close_retries_after_reconnect_before_success() {
    let id = PacodeSessionId::new("ses_close_retry");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let socket = test_socket("close-retry");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let request_order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&request_order);
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        serve_close_connection(first, initial.clone(), true, Arc::clone(&server_order)).await;
        let (second, _) = listener.accept().await.unwrap();
        serve_close_connection(second, initial, false, server_order).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id,
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));

    state.close().await.unwrap();
    assert!(state.closed.load(Ordering::Acquire));
    assert_eq!(
        request_order.lock().unwrap().as_slice(),
        [
            "hello",
            "attach",
            "close_session",
            "hello",
            "attach",
            "close_session"
        ]
    );
    drop(state);
    server.await.unwrap();
}

#[tokio::test]
async fn active_prompt_close_resolves_as_cancelled() {
    let id = PacodeSessionId::new("ses_close_prompt");
    let mut initial = snapshot_state(0);
    initial.meta.id = id.clone();
    let socket = test_socket("close-prompt");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
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
            let name = envelope.req.name().to_string();
            let _ = request_tx.send(name.clone());
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
                    reply: Reply::Attached(initial.clone()),
                },
                Request::GetSnapshot => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Snapshot(initial.clone()),
                },
                Request::UserMessage { .. } => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
                Request::CloseSession => {
                    assert!(
                        write_test_server_message(
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
            assert!(write_test_server_message(&mut write_half, reply).await);
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    let prompt_id = id.clone();
    let connection_task = tokio::spawn(async move {
        agent_client_protocol::Agent
            .builder()
            .connect_with(AcpClient.builder(), async move |cx| {
                let result = prompt_state
                    .run_prompt(
                        &PromptRequest::new(
                            AcpSessionId::new(prompt_id.as_str()),
                            vec![ContentBlock::Text(TextContent::new("run"))],
                        ),
                        &cx,
                    )
                    .await;
                let _ = result_tx.send(result);
                Ok(())
            })
            .await
            .unwrap();
    });
    loop {
        if request_rx.recv().await.as_deref() == Some("user_message") {
            break;
        }
    }
    state.close().await.unwrap();
    assert_eq!(result_rx.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    connection_task.await.unwrap();
    drop(state);
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_completion_waits_for_daemon_acknowledgement() {
    let (state, server) = test_session_state("cancellation-ack").await;
    let generation = state.signal_cancellation();
    let waiter_state = Arc::clone(&state);
    let waiter =
        tokio::spawn(async move { waiter_state.wait_for_interrupt_result(generation).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiter.is_finished());
    state.finish_cancellation(generation, None);
    assert!(waiter.await.unwrap().is_ok());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            state.wait_for_cancellation(generation),
        )
        .await
        .is_err()
    );

    let generation = state.signal_cancellation();
    let waiter_state = Arc::clone(&state);
    let waiter =
        tokio::spawn(async move { waiter_state.wait_for_interrupt_result(generation).await });
    let error = AcpError::Internal("daemon unavailable".into());
    state.finish_cancellation(generation, Some(&error));
    assert!(waiter.await.unwrap().is_err());

    drop(state);
    server.await.unwrap();
}

#[tokio::test]
async fn event_pump_marks_source_close() {
    let socket = test_socket("event-pump");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        assert!(matches!(envelope.req, Request::Hello(_)));
        assert!(
            write_test_server_message(
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
        let mut line = String::new();
        let _ = reader.read_line(&mut line).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let (prompt_sender, _) = tokio::sync::mpsc::channel(1);
    let (task, pump) = spawn_event_pump(
        events,
        sender,
        prompt_sender,
        PacodeSessionId::new("event_pump_test"),
        ClientEventContext::unscoped(),
    );
    drop(client);

    tokio::time::timeout(std::time::Duration::from_secs(2), pump.notify.notified())
        .await
        .unwrap();
    assert!(pump.closed.load(Ordering::Acquire));
    task.abort();
    server.await.unwrap();
}

#[derive(Clone, Copy)]
enum PromptCase {
    IdleWithQueuedTurn,
    ActiveSteer,
    SameTurnEnd,
    SnapshotRetry,
}

async fn run_preflight_prompt(case: PromptCase) -> (Vec<String>, AcpStopReason) {
    let id = PacodeSessionId::new(match case {
        PromptCase::IdleWithQueuedTurn => "ses_idle_preflight",
        PromptCase::ActiveSteer => "ses_active_preflight",
        PromptCase::SameTurnEnd => "ses_same_turn",
        PromptCase::SnapshotRetry => "ses_snapshot_retry",
    });
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    initial.turn_active = matches!(
        case,
        PromptCase::ActiveSteer | PromptCase::SameTurnEnd | PromptCase::SnapshotRetry
    );
    let mut preflight = initial.clone();
    preflight.seq = 3;
    preflight.turn_active = initial.turn_active;
    let socket = test_socket(match case {
        PromptCase::IdleWithQueuedTurn => "idle-preflight",
        PromptCase::ActiveSteer => "active-preflight",
        PromptCase::SameTurnEnd => "same-turn",
        PromptCase::SnapshotRetry => "snapshot-retry",
    });
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let request_order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&request_order);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut snapshot_requests = 0;
        let mut admitted_turn_id = None;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            server_order
                .lock()
                .unwrap()
                .push(envelope.req.name().to_string());
            let mut complete = false;
            let messages = match envelope.req {
                Request::Hello(_) => vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                }],
                Request::Attach(_) => {
                    let mut messages = vec![ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Attached(initial.clone()),
                    }];
                    if matches!(case, PromptCase::IdleWithQueuedTurn) {
                        messages.push(ServerMessage::Event {
                            seq: 1,
                            event: Event::TurnStarted {
                                agent: AgentId::main(),
                                turn: TurnId::new("turn_previous"),
                            },
                        });
                        messages.push(ServerMessage::Event {
                            seq: 2,
                            event: Event::TurnEnded {
                                agent: AgentId::main(),
                                turn: TurnId::new("turn_previous"),
                                usage: None,
                                stop: TurnStop::Completed,
                            },
                        });
                    }
                    messages
                }
                Request::GetSnapshot => {
                    snapshot_requests += 1;
                    if snapshot_requests == 1 {
                        vec![ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Snapshot(preflight.clone()),
                        }]
                    } else {
                        match case {
                            PromptCase::ActiveSteer => {
                                let mut recovered = preflight.clone();
                                recovered.seq = 5;
                                let mut messages = vec![ServerMessage::Reply {
                                    id: envelope.id,
                                    reply: Reply::Snapshot(recovered),
                                }];
                                messages.push(ServerMessage::Event {
                                    seq: 6,
                                    event: Event::TurnStarted {
                                        agent: AgentId::main(),
                                        turn: admitted_turn_id.clone().unwrap(),
                                    },
                                });
                                messages.push(ServerMessage::Event {
                                    seq: 7,
                                    event: Event::TurnEnded {
                                        agent: AgentId::main(),
                                        turn: admitted_turn_id.clone().unwrap(),
                                        usage: None,
                                        stop: TurnStop::Completed,
                                    },
                                });
                                complete = true;
                                messages
                            }
                            PromptCase::SameTurnEnd => {
                                let mut recovered = preflight.clone();
                                recovered.seq = 5;
                                recovered.turn_active = false;
                                let turn = admitted_turn_id.clone().unwrap();
                                complete = true;
                                vec![
                                    ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Snapshot(recovered),
                                    },
                                    ServerMessage::Event {
                                        seq: 6,
                                        event: Event::TurnStarted {
                                            agent: AgentId::main(),
                                            turn: turn.clone(),
                                        },
                                    },
                                    ServerMessage::Event {
                                        seq: 7,
                                        event: Event::TurnEnded {
                                            agent: AgentId::main(),
                                            turn,
                                            usage: None,
                                            stop: TurnStop::Completed,
                                        },
                                    },
                                ]
                            }
                            PromptCase::SnapshotRetry => {
                                if snapshot_requests == 2 {
                                    vec![ServerMessage::Reply {
                                        id: envelope.id,
                                        reply: Reply::Error {
                                            message: "temporary snapshot failure".into(),
                                        },
                                    }]
                                } else {
                                    let mut recovered = preflight.clone();
                                    recovered.seq = 5;
                                    recovered.turn_active = false;
                                    let turn = admitted_turn_id.clone().unwrap();
                                    complete = true;
                                    vec![
                                        ServerMessage::Reply {
                                            id: envelope.id,
                                            reply: Reply::Snapshot(recovered),
                                        },
                                        ServerMessage::Event {
                                            seq: 6,
                                            event: Event::TurnStarted {
                                                agent: AgentId::main(),
                                                turn: turn.clone(),
                                            },
                                        },
                                        ServerMessage::Event {
                                            seq: 7,
                                            event: Event::TurnEnded {
                                                agent: AgentId::main(),
                                                turn,
                                                usage: None,
                                                stop: TurnStop::Completed,
                                            },
                                        },
                                    ]
                                }
                            }
                            PromptCase::IdleWithQueuedTurn => {
                                vec![ServerMessage::Reply {
                                    id: envelope.id,
                                    reply: Reply::Snapshot(preflight.clone()),
                                }]
                            }
                        }
                    }
                }
                Request::UserMessage { message_id, .. } => {
                    admitted_turn_id = Some(admitted_turn(
                        message_id.as_deref().expect("ACP message id"),
                    ));
                    let mut messages = vec![ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    }];
                    match case {
                        PromptCase::IdleWithQueuedTurn => {
                            messages.push(ServerMessage::Event {
                                seq: 4,
                                event: Event::TurnStarted {
                                    agent: AgentId::main(),
                                    turn: admitted_turn_id.clone().unwrap(),
                                },
                            });
                            messages.push(ServerMessage::Event {
                                seq: 5,
                                event: Event::TurnEnded {
                                    agent: AgentId::main(),
                                    turn: admitted_turn_id.clone().unwrap(),
                                    usage: None,
                                    stop: TurnStop::Completed,
                                },
                            });
                            complete = true;
                        }
                        PromptCase::ActiveSteer
                        | PromptCase::SameTurnEnd
                        | PromptCase::SnapshotRetry => {
                            messages.push(ServerMessage::Event {
                                seq: 4,
                                event: Event::TurnEnded {
                                    agent: AgentId::main(),
                                    turn: TurnId::new("turn_a"),
                                    usage: None,
                                    stop: TurnStop::Completed,
                                },
                            });
                        }
                    }
                    messages
                }
                _ => vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                }],
            };
            for message in messages {
                assert!(write_test_server_message(&mut write_half, message).await);
            }
            if complete {
                continue;
            }
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            let _ = stop_tx.send(stop);
            Ok(())
        })
        .await
        .unwrap();
    let stop = tokio::time::timeout(std::time::Duration::from_secs(2), stop_rx)
        .await
        .unwrap()
        .unwrap();
    drop(state);
    server.await.unwrap();
    (request_order.lock().unwrap().clone(), stop)
}

async fn handle_reconnect_idle_connection(
    stream: UnixStream,
    connection: usize,
    initial: SessionSnapshot,
    preflight: SessionSnapshot,
    request_order: Arc<Mutex<Vec<String>>>,
    send_turn_started: bool,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let wire: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
        let envelope = wire.envelope;
        request_order
            .lock()
            .unwrap()
            .push(envelope.req.name().to_string());
        let is_user_message = matches!(&envelope.req, Request::UserMessage { .. });
        let messages = match envelope.req {
            Request::Hello(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            }],
            Request::Attach(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(initial.clone()),
            }],
            Request::GetSnapshot => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Snapshot(preflight.clone()),
            }],
            Request::UserMessage { message_id, .. } => {
                let turn = admitted_turn(message_id.as_deref().expect("ACP message id"));
                let mut messages = vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                }];
                if connection == 1 || send_turn_started {
                    messages.push(ServerMessage::Event {
                        seq: 4,
                        event: Event::TurnStarted {
                            agent: AgentId::main(),
                            turn: turn.clone(),
                        },
                    });
                }
                if connection == 1 {
                    messages.push(ServerMessage::Event {
                        seq: 5,
                        event: Event::TurnEnded {
                            agent: AgentId::main(),
                            turn,
                            usage: None,
                            stop: TurnStop::Completed,
                        },
                    });
                }
                messages
            }
            _ => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            }],
        };
        for message in messages {
            assert!(write_test_server_message(&mut write_half, message).await);
        }
        if is_user_message && connection == 0 {
            return;
        }
    }
}

async fn run_reconnect_idle_prompt(send_turn_started: bool) -> (Vec<String>, AcpStopReason) {
    let id = PacodeSessionId::new("ses_disconnect_idle");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let mut preflight = initial.clone();
    preflight.seq = 3;
    let socket = test_socket("disconnect-idle");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let request_order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&request_order);
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        let first_order = Arc::clone(&server_order);
        let first_initial = initial.clone();
        let first_preflight = preflight.clone();
        let first = tokio::spawn(handle_reconnect_idle_connection(
            first,
            0,
            first_initial,
            first_preflight,
            first_order,
            send_turn_started,
        ));
        let (second, _) = listener.accept().await.unwrap();
        let second_order = Arc::clone(&server_order);
        let second_initial = initial.clone();
        let second_preflight = preflight.clone();
        let second = tokio::spawn(handle_reconnect_idle_connection(
            second,
            1,
            second_initial,
            second_preflight,
            second_order,
            send_turn_started,
        ));
        first.await.unwrap();
        second.await.unwrap();
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            let _ = stop_tx.send(stop);
            Ok(())
        })
        .await
        .unwrap();
    let stop = tokio::time::timeout(std::time::Duration::from_secs(3), stop_rx)
        .await
        .unwrap()
        .unwrap();
    drop(state);
    server.await.unwrap();
    (request_order.lock().unwrap().clone(), stop)
}

async fn handle_idle_burst_connection(
    stream: UnixStream,
    id: PacodeSessionId,
    initial: SessionSnapshot,
    preflight: SessionSnapshot,
    burst_size: u64,
    request_order: Arc<Mutex<Vec<String>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        request_order
            .lock()
            .unwrap()
            .push(envelope.req.name().to_string());
        let messages = match envelope.req {
            Request::Hello(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            }],
            Request::Attach(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(initial.clone()),
            }],
            Request::Interrupt => {
                let mut messages = (1..=burst_size)
                    .map(|seq| ServerMessage::Event {
                        seq,
                        event: Event::SessionStatus {
                            session: id.clone(),
                            status: SessionStatusKind::Working,
                        },
                    })
                    .collect::<Vec<_>>();
                messages.push(ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                });
                messages
            }
            Request::GetSnapshot => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Snapshot(preflight.clone()),
            }],
            Request::UserMessage { message_id, .. } => {
                let turn = admitted_turn(message_id.as_deref().expect("ACP message id"));
                vec![
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    },
                    ServerMessage::Event {
                        seq: burst_size + 2,
                        event: Event::TurnStarted {
                            agent: AgentId::main(),
                            turn: turn.clone(),
                        },
                    },
                    ServerMessage::Event {
                        seq: burst_size + 3,
                        event: Event::TurnEnded {
                            agent: AgentId::main(),
                            turn,
                            usage: None,
                            stop: TurnStop::Completed,
                        },
                    },
                ]
            }
            _ => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            }],
        };
        for message in messages {
            assert!(write_test_server_message(&mut write_half, message).await);
        }
    }
}

async fn run_idle_burst_prompt() -> (Vec<String>, AcpStopReason) {
    let id = PacodeSessionId::new("ses_idle_burst");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let burst_size = EVENT_CHANNEL_CAPACITY as u64 * 4;
    let mut preflight = initial.clone();
    preflight.seq = burst_size + 1;
    let socket = test_socket("idle-burst");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let request_order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&request_order);
    let server_id = id.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_idle_burst_connection(
            stream,
            server_id,
            initial,
            preflight,
            burst_size,
            server_order,
        )
        .await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let context = state.client.event_context();
    state
        .client
        .request_session_for(context, Request::Interrupt)
        .await
        .unwrap();
    assert_eq!(state.events.receiver_count(), 0);
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            let _ = stop_tx.send(stop);
            Ok(())
        })
        .await
        .unwrap();
    let stop = tokio::time::timeout(std::time::Duration::from_secs(3), stop_rx)
        .await
        .unwrap()
        .unwrap();
    drop(state);
    server.await.unwrap();
    (request_order.lock().unwrap().clone(), stop)
}

async fn handle_lagged_prompt_connection(
    stream: UnixStream,
    initial: SessionSnapshot,
    preflight: SessionSnapshot,
    recovered: SessionSnapshot,
    burst_size: u64,
    request_order: Arc<Mutex<Vec<String>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut snapshot_requests = 0;
    let mut user_requests = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        request_order
            .lock()
            .unwrap()
            .push(envelope.req.name().to_string());
        let mut complete = false;
        let messages = match envelope.req {
            Request::Hello(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            }],
            Request::Attach(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(initial.clone()),
            }],
            Request::GetSnapshot => {
                snapshot_requests += 1;
                if snapshot_requests == 1 {
                    let mut messages = (1..=burst_size)
                        .map(|seq| ServerMessage::Event {
                            seq,
                            event: Event::TextDelta {
                                agent: AgentId::main(),
                                item_seq: seq,
                                text: "x".into(),
                            },
                        })
                        .collect::<Vec<_>>();
                    messages.push(ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Snapshot(preflight.clone()),
                    });
                    messages
                } else {
                    vec![ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Snapshot(recovered.clone()),
                    }]
                }
            }
            Request::UserMessage { message_id, .. } => {
                user_requests += 1;
                let mut messages = Vec::new();
                if user_requests == 2 {
                    let turn = admitted_turn(message_id.as_deref().expect("ACP message id"));
                    messages.push(ServerMessage::Event {
                        seq: recovered.seq + 1,
                        event: Event::TurnStarted {
                            agent: AgentId::main(),
                            turn: turn.clone(),
                        },
                    });
                    messages.push(ServerMessage::Event {
                        seq: recovered.seq + 2,
                        event: Event::TurnEnded {
                            agent: AgentId::main(),
                            turn,
                            usage: None,
                            stop: TurnStop::Completed,
                        },
                    });
                    messages.push(ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    });
                    complete = true;
                } else {
                    messages.push(ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    });
                }
                messages
            }
            _ => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            }],
        };
        for message in messages {
            assert!(write_test_server_message(&mut write_half, message).await);
        }
        if complete {
            continue;
        }
    }
}

async fn run_lagged_prompt() -> (Vec<String>, AcpStopReason) {
    let id = PacodeSessionId::new("ses_lagged_broadcast");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let burst_size = EVENT_CHANNEL_CAPACITY as u64 * 4;
    let preflight = initial.clone();
    let mut recovered = initial.clone();
    recovered.seq = burst_size + 1;
    let socket = test_socket("lagged-broadcast");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let request_order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&request_order);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_lagged_prompt_connection(
            stream,
            initial,
            preflight,
            recovered,
            burst_size,
            server_order,
        )
        .await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    let stop = tokio::time::timeout(Duration::from_secs(3), async {
        agent_client_protocol::Agent
            .builder()
            .connect_with(AcpClient.builder(), async move |cx| {
                let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
                let _ = stop_tx.send(stop);
                Ok(())
            })
            .await
            .unwrap();
        stop_rx.await
    })
    .await
    .unwrap()
    .unwrap();
    drop(state);
    server.await.unwrap();
    (request_order.lock().unwrap().clone(), stop)
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_session_burst_is_dropped_until_prompt_preflight() {
    let (request_order, stop) = run_idle_burst_prompt().await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert!(request_order.len() >= 5);
    assert_eq!(
        &request_order[..4],
        ["hello", "attach", "interrupt", "get_snapshot"]
    );
    assert!(
        request_order[4..]
            .iter()
            .all(|request| matches!(request.as_str(), "user_message" | "get_snapshot"))
    );
    assert!(
        request_order[4..]
            .iter()
            .any(|request| request == "user_message")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_prompt_consumer_requests_snapshot_and_recovers_terminal_state() {
    let (request_order, stop) = run_lagged_prompt().await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        &request_order[..5],
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "get_snapshot"
        ]
    );
    assert!(
        request_order[5..]
            .iter()
            .all(|request| matches!(request.as_str(), "user_message" | "get_snapshot"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_idle_snapshot_completes_after_daemon_reconnect() {
    let (request_order, stop) = run_reconnect_idle_prompt(false).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "hello",
            "attach",
            "user_message"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_idle_snapshot_completes_after_confirmed_turn_started() {
    let (request_order, stop) = run_reconnect_idle_prompt(true).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "hello",
            "attach"
        ]
    );
}

#[tokio::test]
async fn queued_previous_turn_is_replayed_before_new_prompt() {
    let (request_order, stop) = run_preflight_prompt(PromptCase::IdleWithQueuedTurn).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        ["hello", "attach", "get_snapshot", "user_message"]
    );
}

#[tokio::test]
async fn old_turn_recovery_waits_for_new_main_turn() {
    let (request_order, stop) = run_preflight_prompt(PromptCase::ActiveSteer).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "get_snapshot",
            "user_message"
        ]
    );
}

#[tokio::test]
async fn same_turn_end_completes_after_idle_snapshot() {
    let (request_order, stop) = run_preflight_prompt(PromptCase::SameTurnEnd).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "get_snapshot",
            "user_message"
        ]
    );
}

#[tokio::test]
async fn turn_end_snapshot_failure_retries_before_completion() {
    let (request_order, stop) = run_preflight_prompt(PromptCase::SnapshotRetry).await;
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        request_order,
        [
            "hello",
            "attach",
            "get_snapshot",
            "user_message",
            "get_snapshot",
            "get_snapshot",
            "user_message"
        ]
    );
}

#[test]
fn evicts_the_least_recently_used_session() {
    let order = ids(&["a", "b", "c"]);
    assert_eq!(evict_candidate(&order, |_| false), Some(0));
}

#[test]
fn skips_a_session_with_a_turn_in_flight() {
    let order = ids(&["a", "b", "c"]);
    let busy: HashSet<String> = ["a".to_string()].into_iter().collect();
    // `a` is the oldest but is mid-turn, so `b` is dropped instead.
    assert_eq!(
        evict_candidate(&order, |id| busy.contains(&id.to_string())),
        Some(1)
    );
}

#[test]
fn refuses_to_evict_when_every_session_is_busy() {
    let order = ids(&["a", "b"]);
    // Going one over the cap beats cutting off a running turn.
    assert_eq!(evict_candidate(&order, |_| true), None);
}

#[test]
fn evict_candidate_of_an_empty_queue_is_none() {
    assert_eq!(evict_candidate(&VecDeque::new(), |_| false), None);
}

#[test]
fn touch_moves_a_session_to_the_most_recent_end() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("a"));
    assert_eq!(names(&order), ["b", "c", "a"]);
}

#[test]
fn touching_the_newest_session_keeps_the_order() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("c"));
    assert_eq!(names(&order), ["a", "b", "c"]);
}

#[test]
fn touching_an_unknown_session_changes_nothing() {
    let mut order = ids(&["a", "b"]);
    touch_order(&mut order, &PacodeSessionId::new("zzz"));
    assert_eq!(names(&order), ["a", "b"]);
}

#[test]
fn a_touched_session_is_no_longer_the_eviction_candidate() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("a"));
    // `a` was the oldest by creation; after use, `b` is the one to drop.
    assert_eq!(evict_candidate(&order, |_| false), Some(0));
    assert_eq!(
        order.front().map(|id| id.to_string()),
        Some("b".to_string())
    );
}

#[test]
fn global_high_seq_does_not_advance_session_floor() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);

    assert!(state.observe(&global_event(1, 1_000)));
    assert!(state.observe(&turn_started(1, 1, "turn_test")));
    assert!(state.observe(&turn_ended(1, 2)));
}

#[test]
fn queued_previous_prompt_events_cannot_finish_next_prompt() {
    let mut state = PromptEventState::new(
        PacodeSessionId::new("ses_test"),
        event_context(1),
        10,
        false,
    );
    let previous_turn = TurnId::new("turn_previous");
    let current_turn = TurnId::new("turn_test");

    assert!(!state.observe(&turn_started(1, 9, "turn_previous")));
    assert!(!state.observe(&turn_ended_for(
        1,
        10,
        AgentId::main(),
        previous_turn.clone(),
    )));
    assert!(!state.ends_active_turn(&AgentId::main(), &previous_turn));
    assert!(state.observe(&turn_started(1, 11, "turn_test")));
    assert!(state.ends_active_turn(&AgentId::main(), &current_turn));
    assert!(state.observe(&turn_ended(1, 12)));
    assert!(state.ends_active_turn(&AgentId::main(), &current_turn));
}

#[test]
fn initial_active_snapshot_waits_for_confirmed_main_turn() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 10, true);
    let old_turn = TurnId::new("turn_a");
    let current_turn = TurnId::new("turn_b");

    assert!(state.observe(&turn_ended_for(1, 11, AgentId::main(), old_turn.clone(),)));
    assert!(!state.ends_active_turn(&AgentId::main(), &old_turn));
    assert!(state.observe(&turn_started(1, 12, "turn_b")));
    assert!(state.ends_active_turn(&AgentId::main(), &current_turn));
    assert!(state.observe(&turn_ended_for(
        1,
        13,
        AgentId::main(),
        current_turn.clone(),
    )));
    assert!(state.complete_turn(&AgentId::main(), &current_turn));
    assert!(!state.ends_active_turn(&AgentId::main(), &current_turn));
}

#[test]
fn reconnect_snapshot_without_turn_id_keeps_new_turn_boundary() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 10, true);
    let old_turn = TurnId::new("turn_a");
    let current_turn = TurnId::new("turn_b");

    assert!(state.observe(&connected(2)));
    assert!(state.observe(&ClientEvent::ResyncNeeded {
        context: event_context(2),
    }));
    let mut active_snapshot = snapshot(2, 11);
    let ClientEvent::Snapshot {
        snapshot: active_snapshot_state,
        ..
    } = &mut active_snapshot
    else {
        unreachable!()
    };
    active_snapshot_state.turn_active = true;
    assert!(state.observe(&active_snapshot));
    assert!(state.observe(&turn_ended_for(2, 12, AgentId::main(), old_turn.clone(),)));
    assert!(!state.ends_active_turn(&AgentId::main(), &old_turn));
    let idle_snapshot = snapshot(2, 13);
    let ClientEvent::Snapshot {
        snapshot: idle_snapshot_state,
        ..
    } = &idle_snapshot
    else {
        unreachable!()
    };
    assert!(state.observe(&idle_snapshot));
    state.mark_prompt_submitted();
    assert!(state.idle_snapshot_stop(idle_snapshot_state).is_none());
    assert!(state.observe(&turn_started(2, 14, "turn_b")));
    assert!(state.ends_active_turn(&AgentId::main(), &current_turn));
    assert!(state.observe(&turn_ended_for(
        2,
        15,
        AgentId::main(),
        current_turn.clone(),
    )));
    assert!(state.complete_turn(&AgentId::main(), &current_turn));
}

#[test]
fn initial_snapshot_state_preserves_active_permissions_and_sequence() {
    let event = snapshot_with_permissions(1, 42, vec![permission_request("perm_initial")]);
    let ClientEvent::Snapshot { snapshot, .. } = event else {
        unreachable!()
    };

    let state = InitialSnapshotState::from(&snapshot);
    assert!(state.turn_active);
    assert_eq!(state.snapshot_seq, 42);
    assert_eq!(state.pending_permissions.len(), 1);
    assert_eq!(
        state.pending_permissions[0].id,
        PermissionId::new("perm_initial")
    );
}

#[tokio::test]
async fn stale_session_context_rejects_session_bound_operation() {
    let (state, server) = test_session_state("stale").await;
    state
        .client
        .attach(Attach::Resume {
            session: PacodeSessionId::new("ses_replacement"),
        })
        .await
        .unwrap();

    assert!(matches!(
        state.interrupt().await,
        Err(AcpError::Client(ClientError::StaleSession))
    ));
    drop(state);
    let _ = server.await;
}

#[tokio::test]
async fn concurrent_insert_keeps_existing_session_arc() {
    let (first, first_server) = test_session_state("first").await;
    let (second, second_server) = test_session_state("second").await;
    let manager = Arc::new(SessionManager::new(ClientOptions::new(
        Paths::under(&std::env::temp_dir()),
        "t",
    )));
    let id = PacodeSessionId::new("ses_shared");
    let first_manager = Arc::clone(&manager);
    let second_manager = Arc::clone(&manager);
    let first_id = id.clone();
    let second_id = id.clone();
    let (first_result, second_result) = tokio::join!(
        async move { first_manager.insert(first_id, first).await },
        async move { second_manager.insert(second_id, second).await },
    );

    assert!(Arc::ptr_eq(&first_result, &second_result));
    let stored = manager.sessions.read().await.get(&id).cloned().unwrap();
    assert!(Arc::ptr_eq(&stored, &first_result));
    drop(stored);
    drop(first_result);
    drop(second_result);
    drop(manager);
    let _ = first_server.await;
    let _ = second_server.await;
}

#[test]
fn attach_failed_invalidates_prompt_state() {
    let id = PacodeSessionId::new("ses_missing");
    let mut state = PromptEventState::new(id.clone(), event_context(1), 0, false);

    assert!(!state.observe(&ClientEvent::AttachFailed {
        context: ClientEventContext {
            connection_epoch: 2,
            session_epoch: 1,
            session: None,
        },
        attach: Attach::Resume { session: id },
        reason: "session not found".into(),
    }));
    assert!(!state.session_snapshot_ready);
    assert!(!state.snapshot_applied);
}

#[test]
fn stale_attach_failure_does_not_invalidate_prompt_state() {
    let id = PacodeSessionId::new("ses_stale_attach");
    let mut state = PromptEventState::new(id.clone(), event_context(2), 0, false);

    assert!(!state.observe(&ClientEvent::AttachFailed {
        context: ClientEventContext {
            connection_epoch: 2,
            session_epoch: 3,
            session: None,
        },
        attach: Attach::Resume { session: id },
        reason: "stale attach".into(),
    }));
    assert!(state.session_snapshot_ready);
    assert!(!state.snapshot_applied);
}

#[test]
fn session_events_wait_for_snapshot_after_reconnect() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);

    assert!(state.observe(&turn_started(1, 100, "turn_old")));
    assert!(state.observe(&connected(2)));
    assert!(!state.observe(&turn_started(2, 101, "turn_new")));
    assert!(state.observe(&unscoped_global_event(2, 1_000)));
    assert!(state.observe(&snapshot(2, 10)));
    assert!(state.observe(&turn_started(2, 11, "turn_new")));
}

#[tokio::test]
async fn repeated_snapshot_replays_only_new_items_and_authoritative_suffix() {
    let (state, server) = test_session_state("snapshot-replay").await;
    let context = state.client.event_context();
    let acp_session_id = AcpSessionId::new(state.id.as_str());
    let assistant = TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "prefix".to_string(),
            complete: false,
        },
    };
    let tool = TranscriptItem {
        seq: 2,
        agent: AgentId::main(),
        ts_ms: 2,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_replay"),
            name: "read_file".to_string(),
            title: "Read file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    let mut baseline = match snapshot(1, 10) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    baseline.meta.id = state.id.clone();
    baseline.transcript = vec![assistant.clone(), tool.clone()];
    let mut suffix = baseline.clone();
    suffix.seq = 11;
    suffix.transcript[0] = TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 3,
        kind: TranscriptKind::Assistant {
            text: "prefix suffix".to_string(),
            complete: false,
        },
    };

    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<SessionNotification>();
    let client = AcpClient.builder().on_receive_notification(
        async move |notification: SessionNotification,
                    _connection: ConnectionTo<agent_client_protocol::Agent>| {
            notification_tx.send(notification).unwrap();
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            let mut event_state =
                PromptEventState::new(prompt_state.id.clone(), context.clone(), 0, false);
            let mut mapping_state = MappingState::new();
            mapping_state.seed_transcript(1, &[(1, "prefix".to_string(), false)]);
            let mut reply =
                |_context: ClientEventContext,
                 _permission: PermissionId,
                 _decision: PermissionDecision,
                 _generation: u64| { std::future::ready(Ok::<(), AcpError>(())) };
            assert!(
                handle_prompt_snapshot(
                    &prompt_state,
                    PromptSnapshot {
                        context: context.clone(),
                        snapshot: &baseline,
                        acp_session_id: &acp_session_id,
                        generation: 0,
                    },
                    &cx,
                    &mut event_state,
                    &mut mapping_state,
                    &mut reply,
                )
                .await
                .unwrap()
            );
            assert!(event_state.accept_resync(&context));
            assert!(
                handle_prompt_snapshot(
                    &prompt_state,
                    PromptSnapshot {
                        context: context.clone(),
                        snapshot: &baseline,
                        acp_session_id: &acp_session_id,
                        generation: 0,
                    },
                    &cx,
                    &mut event_state,
                    &mut mapping_state,
                    &mut reply,
                )
                .await
                .unwrap()
            );
            assert!(event_state.accept_resync(&context));
            assert!(
                handle_prompt_snapshot(
                    &prompt_state,
                    PromptSnapshot {
                        context: context.clone(),
                        snapshot: &suffix,
                        acp_session_id: &acp_session_id,
                        generation: 0,
                    },
                    &cx,
                    &mut event_state,
                    &mut mapping_state,
                    &mut reply,
                )
                .await
                .unwrap()
            );
            tokio::task::yield_now().await;
            Ok(())
        })
        .await
        .unwrap();

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), notification_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first.update, SessionUpdate::ToolCall(_)));
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), notification_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let SessionUpdate::AgentMessageChunk(chunk) = second.update else {
        panic!("expected authoritative assistant suffix")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text suffix")
    };
    assert_eq!(text.text, " suffix");
    assert!(notification_rx.try_recv().is_err());
    drop(state);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_prompts_replay_only_new_transcript_items() {
    let id = PacodeSessionId::new("ses_sequential_replay");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let completed = TranscriptItem {
        seq: 10,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "completed answer".to_string(),
            complete: true,
        },
    };
    let tool = TranscriptItem {
        seq: 12,
        agent: AgentId::main(),
        ts_ms: 3,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new("call_sequential"),
            name: "read_file".to_string(),
            title: "Read file".to_string(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    let mut first_preflight = initial.clone();
    first_preflight.seq = 1;
    let mut second_preflight = first_preflight.clone();
    second_preflight.seq = 6;
    second_preflight.transcript = vec![
        completed.clone(),
        TranscriptItem {
            seq: 11,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::Assistant {
                text: "prefix suffix".to_string(),
                complete: false,
            },
        },
        tool,
    ];
    let socket = test_socket("sequential-replay");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut snapshot_requests = 0;
        let mut prompt_requests = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            let messages = match envelope.req {
                Request::Hello(_) => vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                }],
                Request::Attach(_) => vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(initial.clone()),
                }],
                Request::GetSnapshot => {
                    snapshot_requests += 1;
                    vec![ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Snapshot(if snapshot_requests == 1 {
                            first_preflight.clone()
                        } else {
                            second_preflight.clone()
                        }),
                    }]
                }
                Request::UserMessage { message_id, .. } => {
                    prompt_requests += 1;
                    let first = prompt_requests == 1;
                    let event_seq = if first { 4 } else { 7 };
                    let turn = admitted_turn(message_id.as_deref().expect("ACP message id"));
                    let mut messages = vec![
                        ServerMessage::Reply {
                            id: envelope.id,
                            reply: Reply::Ok,
                        },
                        ServerMessage::Event {
                            seq: event_seq,
                            event: Event::TurnStarted {
                                agent: AgentId::main(),
                                turn: turn.clone(),
                            },
                        },
                        ServerMessage::Event {
                            seq: event_seq + 1,
                            event: Event::TurnEnded {
                                agent: AgentId::main(),
                                turn: turn.clone(),
                                usage: None,
                                stop: TurnStop::Completed,
                            },
                        },
                    ];
                    if first {
                        messages.insert(
                            1,
                            ServerMessage::Event {
                                seq: 2,
                                event: Event::ItemAdded(completed.clone()),
                            },
                        );
                        messages.insert(
                            2,
                            ServerMessage::Event {
                                seq: 3,
                                event: Event::TextDelta {
                                    agent: AgentId::main(),
                                    item_seq: 11,
                                    text: "prefix".to_string(),
                                },
                            },
                        );
                    }
                    messages
                }
                _ => vec![ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                }],
            };
            for message in messages {
                assert!(write_test_server_message(&mut write_half, message).await);
            }
            if prompt_requests == 2 {
                break;
            }
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));

    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<SessionNotification>();
    let client = AcpClient.builder().on_receive_notification(
        async move |notification: SessionNotification,
                    _connection: ConnectionTo<agent_client_protocol::Agent>| {
            notification_tx.send(notification).unwrap();
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );
    let first_prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("first"))],
    );
    let second_prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("second"))],
    );
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            assert_eq!(
                prompt_state.run_prompt(&first_prompt, &cx).await.unwrap(),
                AcpStopReason::EndTurn
            );
            assert_eq!(
                prompt_state.run_prompt(&second_prompt, &cx).await.unwrap(),
                AcpStopReason::EndTurn
            );
            Ok(())
        })
        .await
        .unwrap();

    for expected in ["completed answer", "prefix", " suffix"] {
        let notification =
            tokio::time::timeout(std::time::Duration::from_secs(1), notification_rx.recv())
                .await
                .unwrap()
                .unwrap();
        let SessionUpdate::AgentMessageChunk(chunk) = notification.update else {
            panic!("expected agent message chunk")
        };
        let ContentBlock::Text(text) = chunk.content else {
            panic!("expected text content")
        };
        assert_eq!(text.text, expected);
    }
    let notification =
        tokio::time::timeout(std::time::Duration::from_secs(1), notification_rx.recv())
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(notification.update, SessionUpdate::ToolCall(_)));
    assert!(notification_rx.try_recv().is_err());
    drop(state);
    server.await.unwrap();
}

#[test]
fn preflight_idle_snapshot_does_not_complete_prompt() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    let snapshot_event = snapshot(1, 10);
    let ClientEvent::Snapshot { snapshot, .. } = &snapshot_event else {
        unreachable!()
    };
    state.prepare_prompt_submission(false);
    assert!(state.idle_snapshot_stop(snapshot).is_none());
}

#[test]
fn reconnect_idle_snapshot_completes_submitted_prompt() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    assert!(state.observe(&connected(2)));
    let snapshot_event = snapshot(2, 10);
    let ClientEvent::Snapshot { snapshot, .. } = &snapshot_event else {
        unreachable!()
    };
    assert!(state.observe(&snapshot_event));

    state.prepare_prompt_submission(false);
    assert!(state.idle_snapshot_stop(snapshot).is_none());
    state.mark_prompt_submitted();
    assert!(state.idle_snapshot_stop(snapshot).is_none());
    assert!(state.observe(&turn_started(2, 11, "turn_test")));
    assert_eq!(
        state.idle_snapshot_stop(snapshot),
        Some(AcpStopReason::EndTurn)
    );

    let mut stopped = snapshot.clone();
    stopped.agents = vec![AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Stopped,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("provider", "model"),
        effort: Effort::Low,
        parent: None,
        summary: None,
        error: None,
    }];
    assert_eq!(
        state.idle_snapshot_stop(&stopped),
        Some(AcpStopReason::Cancelled)
    );
}

#[test]
fn pending_new_turn_idle_snapshot_waits_for_terminal_status() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    state.prepare_prompt_submission(true);
    state.mark_prompt_submitted();
    let snapshot_event = snapshot(1, 10);
    let ClientEvent::Snapshot { snapshot, .. } = &snapshot_event else {
        unreachable!()
    };

    assert!(state.idle_snapshot_stop(snapshot).is_none());
    assert!(state.snapshot_terminal_error(snapshot).is_none());
    assert!(state.observe(&turn_started(1, 11, "turn_test")));

    let main_agent = |status, error: Option<&str>| AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("provider", "model"),
        effort: Effort::Low,
        parent: None,
        summary: None,
        error: error.map(str::to_string),
    };
    let mut failed = snapshot.clone();
    failed.agents = vec![main_agent(AgentStatus::Failed, Some("provider failed"))];
    let Some(AcpError::Client(ClientError::Daemon(message))) =
        state.snapshot_terminal_error(&failed)
    else {
        panic!("expected failed snapshot error");
    };
    assert_eq!(message, "provider failed");
    assert!(state.idle_snapshot_stop(&failed).is_none());

    let mut stopped = snapshot.clone();
    stopped.agents = vec![main_agent(AgentStatus::Stopped, None)];
    assert_eq!(
        state.idle_snapshot_stop(&stopped),
        Some(AcpStopReason::Cancelled)
    );
    assert!(state.snapshot_terminal_error(&stopped).is_none());
}

#[test]
fn correlated_snapshot_turn_fences_scope_terminal_error() {
    let expected = TurnId::new("trn_expected");
    let mut state = PromptEventState::new_correlated(
        PacodeSessionId::new("ses_test"),
        event_context(1),
        0,
        false,
        expected.clone(),
        Some(Request::UserMessage {
            text: "run".into(),
            images: Vec::new(),
            message_id: Some("expected".into()),
            attachments: Vec::new(),
        }),
    );
    state.prepare_prompt_submission(false);
    state.mark_prompt_submitted();

    let mut active = snapshot_state(10);
    active.turn_active = true;
    active.active_turn = Some(expected.clone());
    let active_for_stop = active.clone();
    assert!(state.observe(&ClientEvent::Snapshot {
        context: event_context(1),
        snapshot: active,
    }));
    assert!(state.turn_started_confirmed);
    assert!(state.idle_snapshot_stop(&active_for_stop).is_none());

    let mut terminal = snapshot_state(11);
    terminal.terminal_turn = Some(expected.clone());
    terminal.terminal_error = Some("expected failure".into());
    assert!(state.observe(&ClientEvent::Snapshot {
        context: event_context(1),
        snapshot: terminal,
    }));
    let Some(AcpError::Client(ClientError::Daemon(message))) =
        state.snapshot_terminal_error(&snapshot_with_terminal_error(&expected, "expected failure"))
    else {
        panic!("expected authoritative terminal error");
    };
    assert_eq!(message, "expected failure");

    let foreign = snapshot_with_terminal_error(&TurnId::new("trn_foreign"), "foreign failure");
    assert!(state.snapshot_terminal_error(&foreign).is_none());
}

fn snapshot_state(seq: u64) -> SessionSnapshot {
    let ClientEvent::Snapshot { snapshot, .. } = snapshot(1, seq) else {
        unreachable!()
    };
    snapshot
}

fn snapshot_with_terminal_error(turn: &TurnId, error: &str) -> SessionSnapshot {
    let mut snapshot = snapshot_state(12);
    snapshot.terminal_turn = Some(turn.clone());
    snapshot.terminal_error = Some(error.into());
    snapshot
}

#[tokio::test]
async fn divergent_snapshot_replay_closes_session_and_returns_explicit_error() {
    let id = PacodeSessionId::new("ses_divergent_replay");
    let mut initial = snapshot_state(1);
    initial.meta.id = id.clone();
    initial.transcript.push(TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "old".into(),
            complete: false,
        },
    });
    let mut preflight = initial.clone();
    preflight.seq = 2;
    preflight.transcript[0] = TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::Assistant {
            text: "new".into(),
            complete: true,
        },
    };
    let socket = test_socket("divergent-replay");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let order = Arc::new(Mutex::new(Vec::new()));
    let server_order = Arc::clone(&order);
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
            server_order
                .lock()
                .unwrap()
                .push(envelope.req.name().to_string());
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
                    reply: Reply::Attached(initial.clone()),
                },
                Request::GetSnapshot => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Snapshot(preflight.clone()),
                },
                Request::CloseSession => {
                    assert!(
                        write_test_server_message(
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
            assert!(write_test_server_message(&mut write_half, reply).await);
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    let prompt_id = id.clone();
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let result = prompt_state
                .run_prompt(
                    &PromptRequest::new(
                        AcpSessionId::new(prompt_id.as_str()),
                        vec![ContentBlock::Text(TextContent::new("run"))],
                    ),
                    &cx,
                )
                .await;
            let _ = result_tx.send(result);
            Ok(())
        })
        .await
        .unwrap();
    let error = result_rx.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("diverges"));
    assert!(
        order
            .lock()
            .unwrap()
            .iter()
            .any(|name| name == "close_session")
    );
    assert!(
        !order
            .lock()
            .unwrap()
            .iter()
            .any(|name| name == "user_message")
    );
    drop(state);
    server.await.unwrap();
}

#[test]
fn correlated_prompt_rejects_external_main_turn_start() {
    let expected = TurnId::new("trn_expected");
    let mut state = PromptEventState::new_correlated(
        PacodeSessionId::new("ses_test"),
        event_context(1),
        10,
        false,
        expected.clone(),
        Some(Request::UserMessage {
            text: "run".into(),
            images: Vec::new(),
            message_id: Some("expected".into()),
            attachments: Vec::new(),
        }),
    );
    state.prepare_prompt_submission(false);
    state.mark_prompt_submitted();

    assert!(state.observe(&turn_started(1, 11, "trn_external")));
    assert!(!state.turn_started_confirmed);
    assert_eq!(
        state.take_admission_fence().map(|request| request.name()),
        Some("user_message")
    );
    assert!(state.observe(&turn_started(1, 12, "trn_expected")));
    assert!(state.turn_started_confirmed);
}

#[test]
fn equal_seq_idle_snapshot_is_accepted_after_queued_steer_turn_end() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 10, true);
    state.prepare_prompt_submission(true);
    state.mark_prompt_submitted();

    assert!(state.observe(&turn_ended_for(
        1,
        11,
        AgentId::main(),
        TurnId::new("turn_previous"),
    )));
    assert!(state.observe(&snapshot(1, 11)));
    assert!(state.awaiting_new_turn());
    assert!(!state.turn_started_confirmed);
}

#[test]
fn stale_same_session_turn_ended_after_reconnect_is_ignored() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);

    assert!(state.observe(&connected(2)));
    assert!(state.observe(&snapshot(2, 10)));
    assert!(!state.observe(&turn_ended(1, 11)));
}

#[test]
fn valid_post_reconnect_event_is_accepted_after_snapshot_floor() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);

    assert!(state.observe(&turn_started(1, 100, "turn_old")));
    assert!(state.observe(&connected(2)));
    assert!(state.observe(&snapshot(2, 10)));
    assert!(!state.observe(&turn_ended(2, 10)));
    assert!(state.observe(&turn_started(2, 11, "turn_test")));
    assert!(state.observe(&turn_ended(2, 12)));
}

#[test]
fn subagent_turn_ended_does_not_finish_the_main_prompt() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    let main_turn = TurnId::new("turn_test");
    let subagent_turn = TurnId::new("turn_subagent");

    assert!(state.observe(&turn_started(1, 1, "turn_test")));
    assert!(state.observe(&turn_ended_for(
        1,
        2,
        AgentId::new("agt_subagent"),
        subagent_turn,
    )));
    assert!(state.ends_active_turn(&AgentId::main(), &main_turn));
}

#[test]
fn stale_turn_ended_does_not_finish_the_current_prompt() {
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    let current_turn = TurnId::new("turn_test");
    let stale_turn = TurnId::new("turn_previous");

    assert!(state.observe(&turn_started(1, 1, "turn_test")));
    assert!(state.observe(&turn_ended_for(1, 2, AgentId::main(), stale_turn.clone(),)));
    assert!(!state.ends_active_turn(&AgentId::main(), &stale_turn));
    assert!(state.ends_active_turn(&AgentId::main(), &current_turn));
}

#[tokio::test]
async fn reconnect_snapshot_reissues_pending_permission_request() {
    let (session, server) = test_session_state("permission-reissue").await;
    let mut state =
        PromptEventState::new(PacodeSessionId::new("ses_test"), event_context(1), 0, false);
    let snapshot_event = snapshot_with_permissions(2, 10, vec![permission_request("perm_test")]);
    assert!(state.observe(&connected(2)));
    assert!(state.observe(&snapshot_event));
    let ClientEvent::Snapshot { snapshot, .. } = snapshot_event else {
        unreachable!()
    };
    let permissions = snapshot.pending_permissions.clone();

    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let decisions: Arc<Mutex<Vec<(PermissionId, PermissionDecision)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let decisions_for_connection = Arc::clone(&decisions);

    let client = AcpClient.builder().on_receive_request(
        async move |request: RequestPermissionRequest, responder, _cx| {
            request_tx
                .send(request)
                .expect("permission request should be sent");
            responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow_once")),
            ))
        },
        agent_client_protocol::on_receive_request!(),
    );

    let prompt_session = Arc::clone(&session);
    agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            handle_permissions(
                &prompt_session,
                permissions.iter(),
                &AcpSessionId::new("acp_test"),
                event_context(2),
                &cx,
                move |_context, permission, decision| {
                    decisions_for_connection
                        .lock()
                        .expect("decision lock should work")
                        .push((permission, decision));
                    std::future::ready(Ok(()))
                },
            )
            .await
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();

    let request = request_rx
        .recv()
        .await
        .expect("permission request should arrive");
    assert_eq!(request.session_id.0.as_ref(), "acp_test");
    assert_eq!(request.tool_call.tool_call_id.0.as_ref(), "call_test");
    assert_eq!(
        decisions
            .lock()
            .expect("decision lock should work")
            .as_slice(),
        &[(
            PermissionId::new("perm_test"),
            PermissionDecision::AllowOnce
        )]
    );
    drop(session);
    server.await.unwrap();
}

async fn run_permission_wait_barrier(close: bool) {
    let (state, server) = test_session_state(if close {
        "permission-close"
    } else {
        "permission-cancel"
    })
    .await;
    let permission = permission_request("perm_wait");
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = AcpClient.builder().on_receive_request(
        async move |request: RequestPermissionRequest, _responder, _cx| {
            request_tx.send(request).unwrap();
            std::future::pending::<Result<(), agent_client_protocol::Error>>().await
        },
        agent_client_protocol::on_receive_request!(),
    );
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    let permission = permission.clone();
    let connection = tokio::spawn(async move {
        let _ = agent_client_protocol::Agent
            .builder()
            .connect_with(client, async move |cx| {
                let result = handle_permissions_at_generation(
                    &prompt_state,
                    std::iter::once(&permission),
                    &AcpSessionId::new("acp_wait"),
                    prompt_state.client.event_context(),
                    prompt_state.cancellation_generation(),
                    &cx,
                    |_context, _permission, _decision, _generation| std::future::ready(Ok(())),
                )
                .await;
                let _ = done_tx.send(result);
                Ok(())
            })
            .await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), request_rx.recv())
        .await
        .unwrap()
        .unwrap();
    if close {
        state.close().await.unwrap();
    } else {
        state.interrupt().await.unwrap();
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop(state);
    server.await.unwrap();
    connection.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_wait_is_cancelled_by_session_cancel() {
    run_permission_wait_barrier(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_wait_is_cancelled_by_session_close() {
    run_permission_wait_barrier(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resync_marker_recovers_prompt_snapshot_permissions_and_turn_end() {
    let id = PacodeSessionId::new("ses_resync");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let socket = test_socket("resync-prompt");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let (snapshot_request_tx, mut snapshot_request_rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut snapshot_requests = 0;
        let mut admitted_turn_id = None;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            let mut emit_turn_end = false;
            let message = match envelope.req {
                Request::Hello(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
                Request::Attach(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(initial.clone()),
                },
                Request::UserMessage { message_id, .. } => {
                    admitted_turn_id = Some(admitted_turn(
                        message_id.as_deref().expect("ACP message id"),
                    ));
                    assert!(
                        write_test_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id: envelope.id,
                                reply: Reply::Ok,
                            },
                        )
                        .await
                    );
                    assert!(
                        write_test_server_message(
                            &mut write_half,
                            ServerMessage::Resync { current_seq: None },
                        )
                        .await
                    );
                    continue;
                }
                Request::GetSnapshot => {
                    snapshot_requests += 1;
                    let _ = snapshot_request_tx.send(());
                    let mut recovered = initial.clone();
                    recovered.seq = if snapshot_requests == 1 { 3 } else { 4 };
                    recovered.turn_active = true;
                    recovered.transcript = vec![TranscriptItem {
                        seq: recovered.seq,
                        agent: AgentId::main(),
                        ts_ms: recovered.seq,
                        kind: TranscriptKind::Assistant {
                            text: "recovered transcript".into(),
                            complete: true,
                        },
                    }];
                    recovered.pending_permissions = if snapshot_requests == 1 {
                        vec![permission_request("perm_resync")]
                    } else {
                        Vec::new()
                    };
                    emit_turn_end = snapshot_requests == 2;
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Snapshot(recovered),
                    }
                }
                Request::PermissionReply {
                    permission,
                    decision,
                    ..
                } => {
                    assert_eq!(permission, PermissionId::new("perm_resync"));
                    assert_eq!(decision, PermissionDecision::AllowOnce);
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    }
                }
                _ => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
            };
            assert!(write_test_server_message(&mut write_half, message).await);
            if emit_turn_end {
                assert!(
                    write_test_server_message(
                        &mut write_half,
                        ServerMessage::Event {
                            seq: 5,
                            event: Event::TurnStarted {
                                agent: AgentId::main(),
                                turn: admitted_turn_id.clone().unwrap(),
                            },
                        },
                    )
                    .await
                );
                assert!(
                    write_test_server_message(
                        &mut write_half,
                        ServerMessage::Event {
                            seq: 6,
                            event: Event::TurnEnded {
                                agent: AgentId::main(),
                                turn: admitted_turn_id.clone().unwrap(),
                                usage: None,
                                stop: TurnStop::Completed,
                            },
                        },
                    )
                    .await
                );
                continue;
            }
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));

    let (permission_tx, mut permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (notification_tx, mut notification_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = AcpClient
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                permission_tx.send(request).unwrap();
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        "allow_once",
                    )),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: SessionNotification,
                        _connection: ConnectionTo<agent_client_protocol::Agent>| {
                notification_tx.send(notification).unwrap();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        );
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            assert_eq!(stop, AcpStopReason::EndTurn);
            Ok(())
        })
        .await
        .unwrap();

    for _ in 0..2 {
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            snapshot_request_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert!(snapshot_request_rx.try_recv().is_err());
    let permission = tokio::time::timeout(std::time::Duration::from_secs(1), permission_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(permission.session_id.0.as_ref(), id.as_str());
    let notification =
        tokio::time::timeout(std::time::Duration::from_secs(1), notification_rx.recv())
            .await
            .unwrap()
            .unwrap();
    let SessionUpdate::AgentMessageChunk(chunk) = notification.update else {
        panic!("expected replayed assistant message")
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("expected replayed text")
    };
    assert_eq!(text.text, "recovered transcript");

    drop(state);
    server.await.unwrap();
}

async fn handle_session_setup_connection(
    stream: UnixStream,
    snapshot: SessionSnapshot,
    replies: Arc<Mutex<Vec<(PermissionId, PermissionDecision)>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        let message = match envelope.req {
            Request::Hello(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            },
            Request::Attach(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(snapshot.clone()),
            },
            Request::GetSnapshot => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Snapshot(snapshot.clone()),
            },
            Request::PermissionReply {
                permission,
                decision,
                ..
            } => {
                replies.lock().unwrap().push((permission, decision));
                ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                }
            }
            _ => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            },
        };
        assert!(write_test_server_message(&mut write_half, message).await);
    }
}

#[tokio::test]
async fn concurrent_resume_uses_one_attach() {
    let id = PacodeSessionId::new("ses_singleflight_resume");
    let mut attached = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    attached.meta.id = id.clone();
    let socket = test_socket("singleflight-resume");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let replies = Arc::new(Mutex::new(Vec::new()));
    let server_replies = Arc::clone(&replies);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_session_setup_connection(stream, attached, server_replies).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let manager = Arc::new(SessionManager::new(opts));
    let keepalive = Arc::clone(&manager);
    let acp_id = AcpSessionId::new(id.to_string());
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let (first, second) = tokio::join!(
                manager.resume_session(
                    ResumeSessionRequest::new(acp_id.clone(), PathBuf::from("/tmp")),
                    &cx,
                ),
                manager.resume_session(
                    ResumeSessionRequest::new(acp_id, PathBuf::from("/tmp")),
                    &cx,
                ),
            );
            first.unwrap();
            second.unwrap();
            Ok(())
        })
        .await
        .unwrap();

    drop(keepalive);
    server.await.unwrap();
}

#[tokio::test]
async fn load_and_resume_resolve_pending_permissions_once() {
    let id = PacodeSessionId::new("ses_load_permissions");
    let mut attached = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    attached.meta.id = id.clone();
    attached.turn_active = true;
    attached.pending_permissions = vec![
        permission_request("perm_load"),
        permission_request("perm_load"),
    ];
    let socket = test_socket("load-permissions");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let replies = Arc::new(Mutex::new(Vec::new()));
    let server_replies = Arc::clone(&replies);
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        handle_session_setup_connection(first, attached, server_replies).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let manager = Arc::new(SessionManager::new(opts));
    let keepalive = Arc::clone(&manager);
    let acp_id = AcpSessionId::new(id.to_string());
    let permission_requests = Arc::new(Mutex::new(Vec::new()));
    let permission_requests_for_client = Arc::clone(&permission_requests);
    let client = AcpClient.builder().on_receive_request(
        async move |request: RequestPermissionRequest, responder, _cx| {
            permission_requests_for_client.lock().unwrap().push(request);
            responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow_once")),
            ))
        },
        agent_client_protocol::on_receive_request!(),
    );
    agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            let load = manager
                .load_session(
                    LoadSessionRequest::new(acp_id.clone(), PathBuf::from("/tmp")),
                    &cx,
                )
                .await
                .unwrap();
            let resume = manager
                .resume_session(
                    ResumeSessionRequest::new(acp_id, PathBuf::from("/tmp")),
                    &cx,
                )
                .await
                .unwrap();
            assert!(load.modes.is_some());
            assert!(load.config_options.is_none());
            assert!(resume.modes.is_some());
            assert!(resume.config_options.is_none());
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(permission_requests.lock().unwrap().len(), 1);
    assert_eq!(
        replies.lock().unwrap().as_slice(),
        &[(
            PermissionId::new("perm_load"),
            PermissionDecision::AllowOnce
        )]
    );
    drop(keepalive);
    server.await.unwrap();
}

async fn handle_lost_user_message_connection(
    stream: UnixStream,
    connection: usize,
    initial: SessionSnapshot,
    preflight: SessionSnapshot,
    reconnected: SessionSnapshot,
    message_ids: Arc<Mutex<Vec<Option<String>>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let wire: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
        let envelope = wire.envelope;
        let mut complete = false;
        let messages = match envelope.req {
            Request::Hello(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            }],
            Request::Attach(_) => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(if connection == 0 {
                    initial.clone()
                } else {
                    reconnected.clone()
                }),
            }],
            Request::GetSnapshot => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Snapshot(preflight.clone()),
            }],
            Request::UserMessage { message_id, .. } => {
                let turn = admitted_turn(message_id.as_deref().expect("ACP message id"));
                message_ids.lock().unwrap().push(message_id);
                if connection == 0 {
                    return;
                }
                complete = true;
                vec![
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    },
                    ServerMessage::Event {
                        seq: reconnected.seq + 1,
                        event: Event::TurnStarted {
                            agent: AgentId::main(),
                            turn: turn.clone(),
                        },
                    },
                    ServerMessage::Event {
                        seq: reconnected.seq + 2,
                        event: Event::TurnEnded {
                            agent: AgentId::main(),
                            turn,
                            usage: None,
                            stop: TurnStop::Completed,
                        },
                    },
                ]
            }
            _ => vec![ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            }],
        };
        for message in messages {
            assert!(write_test_server_message(&mut write_half, message).await);
        }
        if complete {
            continue;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn user_message_reply_loss_retries_with_same_message_id() {
    let id = PacodeSessionId::new("ses_lost_user_reply");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let mut preflight = initial.clone();
    preflight.seq = 3;
    let mut reconnected = preflight.clone();
    reconnected.turn_active = true;
    let socket = test_socket("lost-user-reply");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let message_ids = Arc::new(Mutex::new(Vec::new()));
    let server_message_ids = Arc::clone(&message_ids);
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        let first = tokio::spawn(handle_lost_user_message_connection(
            first,
            0,
            initial.clone(),
            preflight.clone(),
            reconnected.clone(),
            Arc::clone(&server_message_ids),
        ));
        let (second, _) = listener.accept().await.unwrap();
        let second = tokio::spawn(handle_lost_user_message_connection(
            second,
            1,
            initial,
            preflight,
            reconnected,
            server_message_ids,
        ));
        first.await.unwrap();
        second.await.unwrap();
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            let _ = stop_tx.send(stop);
            Ok(())
        })
        .await
        .unwrap();
    let stop = tokio::time::timeout(std::time::Duration::from_secs(3), stop_rx)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stop, AcpStopReason::EndTurn);
    {
        let message_ids = message_ids.lock().unwrap();
        assert!(message_ids.len() >= 2);
        assert!(message_ids[0].is_some());
        assert!(message_ids.iter().all(|id| id == &message_ids[0]));
    }
    drop(state);
    server.await.unwrap();
}

async fn handle_permission_reply_retry_connection(
    stream: UnixStream,
    connection: usize,
    snapshot: SessionSnapshot,
    replies: Arc<Mutex<Vec<(PermissionId, PermissionDecision)>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let wire: AttachEnvelope = serde_json::from_str(line.trim()).unwrap();
        let envelope = wire.envelope;
        let mut complete = false;
        let message = match envelope.req {
            Request::Hello(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            },
            Request::Attach(_) => {
                complete = connection == 0;
                ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(snapshot.clone()),
                }
            }
            Request::PermissionReply {
                permission,
                decision,
                ..
            } => {
                replies.lock().unwrap().push((permission, decision));
                complete = true;
                ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                }
            }
            _ => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            },
        };
        assert!(write_test_server_message(&mut write_half, message).await);
        if complete {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_reply_retries_on_reconnected_context() {
    let id = PacodeSessionId::new("ses_permission_retry");
    let mut attached = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    attached.meta.id = id.clone();
    let socket = test_socket("permission-retry");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let replies = Arc::new(Mutex::new(Vec::new()));
    let server_replies = Arc::clone(&replies);
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        handle_permission_reply_retry_connection(
            first,
            0,
            attached.clone(),
            Arc::clone(&server_replies),
        )
        .await;
        let (second, _) = listener.accept().await.unwrap();
        handle_permission_reply_retry_connection(second, 1, attached, server_replies).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let stale = client.event_context();
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id,
        stale.clone(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let permission = PermissionId::new("perm_retry");
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        state.reply_permission(stale, permission.clone(), PermissionDecision::AllowOnce),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(state.client.event_context().connection_epoch > 1);
    assert_eq!(
        replies.lock().unwrap().as_slice(),
        &[(permission, PermissionDecision::AllowOnce)]
    );
    server.await.unwrap();
    drop(state);
}

#[derive(Clone)]
struct HistoryPage {
    items: Vec<TranscriptItem>,
    has_more: bool,
}

fn history_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::Assistant {
            text: text.to_string(),
            complete: true,
        },
    }
}

async fn serve_history(
    stream: UnixStream,
    snapshot: SessionSnapshot,
    mut page: impl FnMut(Option<u64>) -> HistoryPage,
    cursors: Arc<Mutex<Vec<Option<u64>>>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
        let reply = match envelope.req {
            Request::Hello(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Hello {
                    daemon_version: "t".to_string(),
                    protocol: PROTOCOL_VERSION,
                    pid: 1,
                },
            },
            Request::Attach(_) => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Attached(snapshot.clone()),
            },
            Request::GetHistory { before_seq, .. } => {
                cursors.lock().unwrap().push(before_seq);
                let served = page(before_seq);
                ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::History {
                        agent: AgentId::main(),
                        items: served.items,
                        has_more: served.has_more,
                    },
                }
            }
            _ => ServerMessage::Reply {
                id: envelope.id,
                reply: Reply::Ok,
            },
        };
        assert!(write_test_server_message(&mut write_half, reply).await);
    }
}

fn history_load_snapshot(id: &PacodeSessionId, first_seq: u64) -> SessionSnapshot {
    let mut attached = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    attached.meta.id = id.clone();
    attached.has_more_history = true;
    attached.transcript = vec![history_item(first_seq, "snapshot tail")];
    attached
}

async fn run_history_load(
    name: &str,
    first_seq: u64,
    page: impl FnMut(Option<u64>) -> HistoryPage + Send + 'static,
) -> (
    Result<LoadSessionResponse, String>,
    Vec<Option<u64>>,
    usize,
    JoinHandle<()>,
) {
    let id = PacodeSessionId::new(name);
    let attached = history_load_snapshot(&id, first_seq);
    let socket = test_socket(name);
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let cursors = Arc::new(Mutex::new(Vec::new()));
    let server_cursors = Arc::clone(&cursors);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve_history(stream, attached, page, server_cursors).await;
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let manager = Arc::new(SessionManager::new(opts));
    let keepalive = Arc::clone(&manager);
    let acp_id = AcpSessionId::new(id.as_str());
    let notifications = Arc::new(AtomicUsize::new(0));
    let client_notifications = Arc::clone(&notifications);
    let client = AcpClient.builder().on_receive_notification(
        async move |_notification: SessionNotification,
                    _connection: ConnectionTo<agent_client_protocol::Agent>| {
            client_notifications.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let _ = agent_client_protocol::Agent
        .builder()
        .connect_with(client, async move |cx| {
            let result = manager
                .load_session(LoadSessionRequest::new(acp_id, PathBuf::from("/tmp")), &cx)
                .await
                .map_err(|error| error.to_string());
            let _ = result_tx.send(result);
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        })
        .await;
    let result = result_rx.await.unwrap();
    drop(keepalive);
    (
        result,
        cursors.lock().unwrap().clone(),
        notifications.load(Ordering::Relaxed),
        server,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_terminal_history_page_finishes_the_load() {
    let (result, cursors, notifications, server) =
        run_history_load("history-empty-terminal", 7, |_before| HistoryPage {
            items: Vec::new(),
            has_more: false,
        })
        .await;
    result.unwrap();
    assert_eq!(cursors, [Some(7)]);
    assert_eq!(notifications, 1, "the snapshot window is still replayed");
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn history_page_claiming_more_after_an_empty_page_is_rejected() {
    let (result, _cursors, _notifications, server) =
        run_history_load("history-empty-lying", 7, |_before| HistoryPage {
            items: Vec::new(),
            has_more: true,
        })
        .await;
    assert!(result.is_err());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn history_replay_stops_at_the_page_cap() {
    let window_first = (MAX_HISTORY_PAGES * HISTORY_PAGE_LIMIT * 8) as u64;
    let (result, cursors, notifications, server) =
        run_history_load("history-capped", window_first, move |before_seq| {
            let high = before_seq.unwrap_or(0);
            let low = high.saturating_sub(HISTORY_PAGE_LIMIT as u64);
            if low < 2 {
                return HistoryPage {
                    items: Vec::new(),
                    has_more: false,
                };
            }
            HistoryPage {
                items: (low..high)
                    .map(|seq| history_item(seq, &format!("history {seq}")))
                    .collect(),
                has_more: low > 2,
            }
        })
        .await;
    result.unwrap();
    assert_eq!(
        cursors.len(),
        MAX_HISTORY_PAGES * 2,
        "the backward walk and the forward pass each stop at the cap"
    );
    assert_eq!(cursors[0], Some(window_first));
    assert_eq!(notifications, MAX_HISTORY_PAGES * HISTORY_PAGE_LIMIT + 1);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_prompt_submission_never_holds_the_submission_lock() {
    let id = PacodeSessionId::new("ses_cancel_submission");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let mut preflight = initial.clone();
    preflight.seq = 2;
    let socket = test_socket("cancel-submission");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = Arc::clone(&requests);
    let (snapshot_seen_tx, mut snapshot_seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, mut release_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
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
            server_requests
                .lock()
                .unwrap()
                .push(envelope.req.name().to_string());
            if matches!(envelope.req, Request::GetSnapshot) {
                snapshot_seen_tx.send(()).unwrap();
                release_rx.recv().await.unwrap();
            }
            let reply = match envelope.req {
                Request::Hello(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
                Request::Attach(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(initial.clone()),
                },
                Request::GetSnapshot => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Snapshot(preflight.clone()),
                },
                _ => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
            };
            assert!(write_test_server_message(&mut write_half, reply).await);
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));

    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    let turn = tokio::spawn(async move {
        let _ = agent_client_protocol::Agent
            .builder()
            .connect_with(AcpClient.builder(), async move |cx| {
                let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
                let _ = stop_tx.send(stop);
                Ok(())
            })
            .await;
    });

    tokio::time::timeout(Duration::from_secs(2), snapshot_seen_rx.recv())
        .await
        .unwrap()
        .unwrap();
    state.signal_cancellation();
    release_tx.send(()).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while state.cancellation_tx.receiver_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the prompt never waited for its cancellation"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        state.submission_lock.try_lock().is_ok(),
        "the prompt must not await its own cancellation while holding the lock its canceller needs"
    );

    let interrupt_state = Arc::clone(&state);
    tokio::time::timeout(Duration::from_secs(2), async move {
        interrupt_state.interrupt().await.unwrap();
    })
    .await
    .unwrap();
    let stop = tokio::time::timeout(Duration::from_secs(2), stop_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stop, AcpStopReason::Cancelled);
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|name| name == "user_message"),
        "a cancelled prompt must not submit the user message"
    );

    turn.await.unwrap();
    drop(state);
    server.await.unwrap();
}

#[tokio::test]
async fn captured_context_never_moves_the_session_backwards() {
    let (state, server) = test_session_state("context-fence").await;
    let mut newer = event_context(9);
    newer.session_epoch = 11;
    *state.context.lock().await = newer.clone();

    let captured = state.captured_context().await;
    assert_eq!(captured.connection_epoch, 1, "the live context still wins");
    assert_eq!(
        *state.context.lock().await,
        newer,
        "an older live context must not overwrite a newer stored one"
    );
    assert!(context_not_older(&newer, &event_context(1)));
    assert!(!context_not_older(&event_context(1), &newer));
    drop(state);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_snapshot_never_downgrades_the_stored_session_context() {
    let id = PacodeSessionId::new("ses_snapshot_fence");
    let mut initial = match snapshot(1, 0) {
        ClientEvent::Snapshot { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    initial.meta.id = id.clone();
    let mut preflight = initial.clone();
    preflight.seq = 3;
    let socket = test_socket("snapshot-fence");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_path = socket.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let mut turn: Option<TurnId> = None;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_str(line.trim()).unwrap();
            let mut events = Vec::new();
            let reply = match envelope.req {
                Request::Hello(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Hello {
                        daemon_version: "t".to_string(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    },
                },
                Request::Attach(_) => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Attached(initial.clone()),
                },
                Request::GetSnapshot => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Snapshot(preflight.clone()),
                },
                Request::UserMessage { message_id, .. } => {
                    let admitted = admitted_turn(message_id.as_deref().expect("ACP message id"));
                    turn = Some(admitted.clone());
                    events.push(4);
                    events.push(5);
                    ServerMessage::Reply {
                        id: envelope.id,
                        reply: Reply::Ok,
                    }
                }
                _ => ServerMessage::Reply {
                    id: envelope.id,
                    reply: Reply::Ok,
                },
            };
            assert!(write_test_server_message(&mut write_half, reply).await);
            let Some(admitted) = turn.as_ref() else {
                continue;
            };
            for seq in events {
                let event = if seq == 4 {
                    Event::TurnStarted {
                        agent: AgentId::main(),
                        turn: admitted.clone(),
                    }
                } else {
                    Event::TurnEnded {
                        agent: AgentId::main(),
                        turn: admitted.clone(),
                        usage: None,
                        stop: TurnStop::Completed,
                    }
                };
                assert!(
                    write_test_server_message(&mut write_half, ServerMessage::Event { seq, event })
                        .await
                );
            }
        }
        let _ = std::fs::remove_file(socket_path);
    });

    let mut opts = ClientOptions::new(Paths::under(&std::env::temp_dir()), "t");
    opts.socket = Some(socket);
    opts.spawn_daemon = false;
    let (client, mut events) = Client::connect(opts).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Connected { .. })
    ));
    let attached = client
        .attach(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    let state = Arc::new(SessionState::new(
        id.clone(),
        client.event_context(),
        client,
        events,
        InitialSnapshotState::from(&attached),
        SessionConfigState {
            meta: attached.meta.clone(),
            models: Vec::new(),
        },
        ConfigOptionCaps::default(),
    ));
    let mut newer = event_context(9);
    newer.session_epoch = 11;
    *state.context.lock().await = newer.clone();

    let prompt = PromptRequest::new(
        AcpSessionId::new(id.as_str()),
        vec![ContentBlock::Text(TextContent::new("run"))],
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let prompt_state = Arc::clone(&state);
    agent_client_protocol::Agent
        .builder()
        .connect_with(AcpClient.builder(), async move |cx| {
            let stop = prompt_state.run_prompt(&prompt, &cx).await.unwrap();
            let _ = stop_tx.send(stop);
            Ok(())
        })
        .await
        .unwrap();
    let stop = tokio::time::timeout(Duration::from_secs(2), stop_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stop, AcpStopReason::EndTurn);
    assert_eq!(
        *state.context.lock().await,
        newer,
        "a snapshot fetched on the live connection must not move a newer stored context back"
    );

    drop(state);
    server.await.unwrap();
}
