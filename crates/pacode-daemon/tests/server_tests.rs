//! Integration tests for pacode-daemon.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use pacode_core::{Core, CoreDeps};
use pacode_daemon::connection::{ServerControl, serve_connection};
use pacode_daemon::{DaemonOptions, build_core, run, server::bind_socket};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_plugin::marketplace::{FetchError, HttpFetcher, Marketplace, MarketplaceFetcher};
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::builtin_tools;
use pacode_types::protocol::{
    Attach, AttachEnvelope, ClientHello, Envelope, Event, PROTOCOL_VERSION, Reply, Request,
    ServerMessage,
};
use pacode_types::{
    ClientId, Config, ExecConfig, Mode, ModelRoute, SessionId, SessionStatusKind, WebConfig, now_ms,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

async fn send_env<W: AsyncWriteExt + Unpin>(writer: &mut W, env: &Envelope) {
    let mut line = serde_json::to_string(env).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

async fn send_attach<W: AsyncWriteExt + Unpin>(writer: &mut W, env: &AttachEnvelope) {
    let mut line = serde_json::to_string(env).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

async fn read_msg<R: AsyncBufReadExt + Unpin>(lines: &mut Lines<R>) -> ServerMessage {
    let line = lines
        .next_line()
        .await
        .unwrap()
        .expect("expected line from server");
    serde_json::from_str(&line).unwrap()
}

async fn read_reply<R: AsyncBufReadExt + Unpin>(lines: &mut Lines<R>, expected_id: u64) -> Reply {
    loop {
        let msg = read_msg(lines).await;
        if let ServerMessage::Reply { id, reply } = msg
            && id == expected_id
        {
            return reply;
        }
    }
}

async fn wait_for_session_unload(core: &Core, session: &SessionId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.session(session).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("session {session} stayed resident after its last lease was released")
    });
}

async fn start_test_daemon(
    socket: &Path,
    paths: pacode_config::Paths,
    config: Arc<Config>,
    core: Arc<Core>,
) -> tokio::task::JoinHandle<Result<(), pacode_daemon::DaemonError>> {
    let task = tokio::spawn(run(
        DaemonOptions {
            paths,
            config,
            socket: socket.to_path_buf(),
            app_version: "0.1.0".to_string(),
        },
        core,
    ));
    for _ in 0..100 {
        if socket.exists() {
            return task;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("daemon did not bind {}", socket.display());
}

async fn initialize_client<W, R>(writer: &mut W, lines: &mut Lines<R>, client_id: ClientId)
where
    W: AsyncWriteExt + Unpin,
    R: AsyncBufReadExt + Unpin,
{
    send_env(
        writer,
        &Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id,
                app_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION,
            }),
        },
    )
    .await;
    assert!(matches!(read_reply(lines, 1).await, Reply::Hello { .. }));
}

async fn build_test_core(tmp: &tempfile::TempDir, config: Arc<Config>) -> Arc<Core> {
    let paths = pacode_config::Paths::under(tmp.path());
    let marketplace = Arc::new(Marketplace::new(
        Arc::new(HttpFetcher::new()),
        paths.cache_dir.join("marketplace"),
        paths.cache_dir.join("plugins"),
        pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
    ));
    build_test_core_with_marketplace(tmp, config, marketplace).await
}

async fn build_test_core_with_marketplace(
    tmp: &tempfile::TempDir,
    config: Arc<Config>,
    marketplace: Arc<Marketplace>,
) -> Arc<Core> {
    let mock = Arc::new(MockProvider::new("mock"));
    mock.push(MockResponse::Text("Reply 1".to_string()));
    mock.push(MockResponse::Text("Reply 2".to_string()));
    mock.push(MockResponse::Text("Reply 3".to_string()));
    build_test_core_with_marketplace_and_mock(tmp, config, marketplace, mock).await
}

async fn build_test_core_with_marketplace_and_mock(
    tmp: &tempfile::TempDir,
    config: Arc<Config>,
    marketplace: Arc<Marketplace>,
    mock: Arc<MockProvider>,
) -> Arc<Core> {
    let paths = pacode_config::Paths::under(tmp.path());
    let store = pacode_daemon::wiring::shared_store(&paths.db_file()).unwrap();
    let tasks = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let mcp = McpPool::new(BTreeMap::new(), None, None);

    let mut providers = ProviderRegistry::empty();
    providers.insert(mock);
    providers.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let tools = builtin_tools(&WebConfig::default());

    let deps = CoreDeps {
        marketplace,
        config,
        paths,
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp,
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store,
        app_version: "0.1.0".to_string(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };

    Core::new(deps).await
}

struct BlockingMarketplaceFetcher {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl MarketplaceFetcher for BlockingMarketplaceFetcher {
    fn get<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _url: &'life1 str,
        _limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let started = Arc::clone(&self.started);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            started.notify_one();
            release.notified().await;
            Ok(br#"{"plugins":[]}"#.to_vec())
        })
    }
}

#[tokio::test]
async fn bind_socket_removes_stale_socket_file() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("stale.sock");

    // Create a stale file where the socket should be
    std::fs::write(&sock_path, b"stale socket").unwrap();
    assert!(sock_path.exists());

    // bind_socket should probe connect, find it refused, remove it, and bind successfully
    let listener = bind_socket(&sock_path)
        .await
        .expect("bind_socket should remove stale socket and bind");

    // Verify the socket is active now
    assert!(sock_path.exists());

    // If we try to bind again while the listener is listening, it should fail with SocketBusy
    let err = bind_socket(&sock_path).await.unwrap_err();
    match err {
        pacode_daemon::DaemonError::SocketBusy { path } => {
            assert_eq!(path, sock_path);
        }
        other => panic!("expected SocketBusy, got {other:?}"),
    }

    drop(listener);
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_with_protocol_999_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_proto.sock");

    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let opts = DaemonOptions {
        paths,
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };

    let server_task = tokio::spawn(run(opts, core));

    // Wait briefly for the server to bind
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let hello_req = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: ClientId::generate(),
            app_version: "0.1.0".to_string(),
            protocol: 999,
        }),
    };

    send_env(&mut writer, &hello_req).await;

    let reply_msg = read_msg(&mut lines).await;
    match reply_msg {
        ServerMessage::Reply { id, reply } => {
            assert_eq!(id, 1);
            match reply {
                Reply::Error { message } => {
                    assert!(
                        message.contains("protocol"),
                        "error message should mention protocol mismatch: {message}"
                    );
                }
                other => panic!("expected Reply::Error, got {other:?}"),
            }
        }
        other => panic!("expected Reply, got {other:?}"),
    }

    // Connection should be closed by daemon
    let next = lines.next_line().await.unwrap();
    assert!(
        next.is_none(),
        "connection should be closed after protocol error"
    );

    server_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_client_is_closed_after_handshake_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("handshake-timeout.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

    let silent = UnixStream::connect(&socket).await.unwrap();
    let (silent_reader, silent_writer) = silent.into_split();
    let mut silent_lines = BufReader::new(silent_reader).lines();
    let reply: Reply =
        tokio::time::timeout(Duration::from_secs(7), read_reply(&mut silent_lines, 0))
            .await
            .expect("handshake did not time out");
    assert!(matches!(
        reply,
        Reply::Error { message } if message.contains("handshake timed out")
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), silent_lines.next_line())
            .await
            .expect("silent connection stayed open")
            .expect("silent connection read failed")
            .is_none()
    );
    drop(silent_writer);
    drop(silent_lines);

    let admin = UnixStream::connect(&socket).await.unwrap();
    let (admin_reader, mut admin_writer) = admin.into_split();
    let mut admin_lines = BufReader::new(admin_reader).lines();
    initialize_client(&mut admin_writer, &mut admin_lines, ClientId::generate()).await;
    send_env(
        &mut admin_writer,
        &Envelope {
            id: 2,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut admin_lines, 2).await, Reply::Ok);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("daemon did not stop")
            .expect("daemon task join failed")
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_steady_input_line_is_rejected_and_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("input-line-overflow.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    initialize_client(&mut writer, &mut lines, ClientId::generate()).await;
    let write_task = tokio::spawn(async move {
        let payload = vec![b'x'; 8 * 1024 * 1024 + 1];
        let _ = writer.write_all(&payload).await;
        let _ = writer.flush().await;
    });
    let reply: Reply = tokio::time::timeout(Duration::from_secs(5), read_reply(&mut lines, 0))
        .await
        .expect("oversized line was not rejected");
    assert!(matches!(
        reply,
        Reply::Error { message } if message.contains("NDJSON line too long")
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), lines.next_line())
            .await
            .expect("oversized-input connection stayed open")
            .expect("oversized-input connection read failed")
            .is_none()
    );
    write_task.abort();
    let _ = write_task.await;

    let admin = UnixStream::connect(&socket).await.unwrap();
    let (admin_reader, mut admin_writer) = admin.into_split();
    let mut admin_lines = BufReader::new(admin_reader).lines();
    initialize_client(&mut admin_writer, &mut admin_lines, ClientId::generate()).await;
    send_env(
        &mut admin_writer,
        &Envelope {
            id: 2,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut admin_lines, 2).await, Reply::Ok);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("daemon did not stop")
            .expect("daemon task join failed")
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_admission_rejects_clients_above_the_limit() {
    const ADMISSION_LIMIT: usize = 64;

    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("connection-admission.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

    let mut admitted = Vec::with_capacity(ADMISSION_LIMIT);
    for _ in 0..ADMISSION_LIMIT {
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        initialize_client(&mut writer, &mut lines, ClientId::generate()).await;
        admitted.push((writer, lines));
    }

    let rejected = UnixStream::connect(&socket).await.unwrap();
    let (rejected_reader, _rejected_writer) = rejected.into_split();
    let mut rejected_lines = BufReader::new(rejected_reader).lines();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rejected_lines.next_line())
            .await
            .expect("rejected connection stayed open")
            .expect("rejected connection read failed")
            .is_none()
    );

    let (mut writer, mut lines) = admitted.pop().unwrap();
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 2).await, Reply::Ok);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("daemon did not stop")
            .expect("daemon task join failed")
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn full_flow_hello_attach_message_events_snapshot_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_flow.sock");

    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let opts = DaemonOptions {
        paths,
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };

    let server_task = tokio::spawn(run(opts, core.clone()));

    // Wait for the socket to appear
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // 1. Hello -> Hello reply
    let hello_req = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: ClientId::generate(),
            app_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    send_env(&mut writer, &hello_req).await;

    let reply = read_reply(&mut lines, 1).await;
    match reply {
        Reply::Hello {
            daemon_version,
            protocol,
            pid,
        } => {
            assert_eq!(daemon_version, "0.1.0");
            assert_eq!(protocol, PROTOCOL_VERSION);
            assert!(pid > 0);
        }
        other => panic!("expected Reply::Hello, got {other:?}"),
    }

    // 2. Attach New{cwd: tmp} -> Attached with meta
    let attach = Attach::New {
        cwd: tmp.path().to_path_buf(),
        model: None,
        effort: None,
        mode: None,
    };
    let attach_req = AttachEnvelope::new(2, attach.clone(), "flow-new");
    send_attach(&mut writer, &attach_req).await;

    let reply = read_reply(&mut lines, 2).await;
    let session_a = match reply {
        Reply::Attached(snapshot) => {
            assert_eq!(snapshot.meta.cwd, tmp.path());
            snapshot.meta.id
        }
        other => panic!("expected Reply::Attached, got {other:?}"),
    };

    let attach_retry = AttachEnvelope::new(22, attach, "flow-new");
    send_attach(&mut writer, &attach_retry).await;
    let reply = read_reply(&mut lines, 22).await;
    match reply {
        Reply::Attached(snapshot) => assert_eq!(snapshot.meta.id, session_a),
        other => panic!("expected idempotent Reply::Attached, got {other:?}"),
    }
    assert_eq!(core.session_ids().len(), 1);

    // 3. UserMessage -> Ok, then Events including text_delta and turn_ended
    let msg_req = Envelope {
        id: 3,
        req: Request::UserMessage {
            text: "Hello agent".to_string(),
            images: Vec::new(),
            message_id: None,
        },
    };
    send_env(&mut writer, &msg_req).await;

    let reply = read_reply(&mut lines, 3).await;
    match reply {
        Reply::Ok => {}
        other => panic!("expected Reply::Ok, got {other:?}"),
    }

    let mut saw_text_delta = false;
    let mut saw_turn_ended = false;

    // Read events until TurnEnded
    while !saw_turn_ended {
        let msg = read_msg(&mut lines).await;
        if let ServerMessage::Event { seq: _, event } = msg {
            match event {
                pacode_types::protocol::Event::TextDelta { .. } => {
                    saw_text_delta = true;
                }
                pacode_types::protocol::Event::TurnEnded { .. } => {
                    saw_turn_ended = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        saw_text_delta,
        "should have received at least one TextDelta event"
    );
    assert!(saw_turn_ended, "should have received TurnEnded event");

    // 4. GetSnapshot -> Snapshot
    let snap_req = Envelope {
        id: 4,
        req: Request::GetSnapshot,
    };
    send_env(&mut writer, &snap_req).await;

    let reply = read_reply(&mut lines, 4).await;
    match reply {
        Reply::Snapshot(snapshot) => {
            assert_eq!(snapshot.meta.cwd, tmp.path());
        }
        other => panic!("expected Reply::Snapshot, got {other:?}"),
    }

    let close_req = Envelope {
        id: 5,
        req: Request::CloseSession,
    };
    send_env(&mut writer, &close_req).await;
    let reply = read_reply(&mut lines, 5).await;
    assert_eq!(reply, Reply::Ok);
    assert!(core.session_ids().is_empty());

    let snap_after_close = Envelope {
        id: 6,
        req: Request::GetSnapshot,
    };
    send_env(&mut writer, &snap_after_close).await;
    assert!(matches!(
        read_reply(&mut lines, 6).await,
        Reply::Error { .. }
    ));

    let shut_req = Envelope {
        id: 7,
        req: Request::Shutdown { force: true },
    };
    send_env(&mut writer, &shut_req).await;

    let reply = read_reply(&mut lines, 7).await;
    match reply {
        Reply::Ok => {}
        other => panic!("expected Reply::Ok, got {other:?}"),
    }

    // Wait for the server task to finish
    let server_res = tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
        .await
        .expect("server did not shut down in time")
        .expect("server task join error");
    assert!(
        server_res.is_ok(),
        "server::run returned error: {server_res:?}"
    );

    // Assert socket file is gone
    assert!(
        !sock_path.exists(),
        "socket file should be deleted after shutdown"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn latest_attach_retry_keeps_the_first_selected_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_latest_retry.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let client_id = ClientId::new("latest-restart-client");
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let first = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: None,
            effort: None,
            mode: None,
        })
        .await
        .unwrap();

    let opts = DaemonOptions {
        paths: paths.clone(),
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };
    let server_task = tokio::spawn(run(opts, core.clone()));
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    send_env(
        &mut writer,
        &Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: client_id.clone(),
                app_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION,
            }),
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut lines, 1).await,
        Reply::Hello { .. }
    ));

    let attach = Attach::Latest {
        cwd: tmp.path().to_path_buf(),
    };
    send_attach(
        &mut writer,
        &AttachEnvelope::new(2, attach.clone(), "latest-flow"),
    )
    .await;
    let selected = match read_reply(&mut lines, 2).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached, got {other:?}"),
    };
    assert_eq!(selected, first);

    let first_updated_at = core.session(&first).unwrap().meta().updated_at_ms;
    while now_ms() <= first_updated_at {
        tokio::task::yield_now().await;
    }
    let newer = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: None,
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    assert_eq!(core.open_session(attach.clone()).await.unwrap(), newer);

    send_attach(
        &mut writer,
        &AttachEnvelope::new(3, attach.clone(), "latest-flow"),
    )
    .await;
    let retried = match read_reply(&mut lines, 3).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached, got {other:?}"),
    };
    assert_eq!(retried, first);
    assert!(core.session(&first).is_some());
    assert!(core.session(&newer).is_some());
    assert_eq!(core.session_ids().len(), 2);

    send_env(
        &mut writer,
        &Envelope {
            id: 4,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert!(matches!(read_reply(&mut lines, 4).await, Reply::Ok));
    let first_result = tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
        .await
        .expect("latest daemon did not stop")
        .expect("latest daemon join failed");
    assert!(first_result.is_ok());
    drop(writer);
    drop(lines);

    let restarted_core = build_test_core(&tmp, Arc::clone(&config)).await;
    let restarted_task = start_test_daemon(
        &sock_path,
        paths,
        Arc::clone(&config),
        restarted_core.clone(),
    )
    .await;
    let restarted_stream = UnixStream::connect(&sock_path).await.unwrap();
    let (restarted_reader, mut restarted_writer) = restarted_stream.into_split();
    let mut restarted_lines = BufReader::new(restarted_reader).lines();
    initialize_client(&mut restarted_writer, &mut restarted_lines, client_id).await;
    send_attach(
        &mut restarted_writer,
        &AttachEnvelope::new(5, attach, "latest-flow"),
    )
    .await;
    match read_reply(&mut restarted_lines, 5).await {
        Reply::Attached(snapshot) => assert_eq!(snapshot.meta.id, first),
        other => panic!("expected recorded latest session after restart, got {other:?}"),
    }
    assert_eq!(restarted_core.session_ids(), vec![first.clone()]);

    send_env(
        &mut restarted_writer,
        &Envelope {
            id: 6,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut restarted_lines, 6).await,
        Reply::Ok
    ));
    let restarted_result = tokio::time::timeout(std::time::Duration::from_secs(5), restarted_task)
        .await
        .expect("restarted daemon did not stop")
        .expect("restarted daemon join failed");
    assert!(restarted_result.is_ok());
}

#[tokio::test]
async fn build_core_with_mock_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;

    // Set PACODE_MOCK_PROVIDER
    unsafe {
        std::env::set_var("PACODE_MOCK_PROVIDER", "1");
    }

    let opts = DaemonOptions {
        paths,
        config: Arc::new(config),
        socket: tmp.path().join("daemon.sock"),
        app_version: "0.1.0".to_string(),
    };
    let core_res = build_core(&opts).await;
    unsafe {
        std::env::remove_var("PACODE_MOCK_PROVIDER");
    }

    assert!(core_res.is_ok(), "build_core failed");
    let core = core_res.unwrap();
    assert!(core.is_idle());
}

#[tokio::test(flavor = "multi_thread")]
async fn unattached_requests_behavior() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_unattached.sock");

    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let opts = DaemonOptions {
        paths,
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };

    let server_task = tokio::spawn(run(opts, core));

    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // 1. Hello
    let hello_req = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: ClientId::generate(),
            app_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    send_env(&mut writer, &hello_req).await;
    let reply = read_reply(&mut lines, 1).await;
    assert!(matches!(reply, Reply::Hello { .. }));

    // 2. Ping -> Pong
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Ping,
        },
    )
    .await;
    let reply = read_reply(&mut lines, 2).await;
    assert_eq!(reply, Reply::Pong);

    // 3. Detach -> Ok
    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::Detach,
        },
    )
    .await;
    let reply = read_reply(&mut lines, 3).await;
    assert_eq!(reply, Reply::Ok);

    // 4. GetSnapshot without attach -> Error
    send_env(
        &mut writer,
        &Envelope {
            id: 4,
            req: Request::GetSnapshot,
        },
    )
    .await;
    let reply = read_reply(&mut lines, 4).await;
    assert!(matches!(reply, Reply::Error { .. }));

    // 5. UserMessage without attach -> Error
    send_env(
        &mut writer,
        &Envelope {
            id: 5,
            req: Request::UserMessage {
                text: "test".to_string(),
                images: Vec::new(),
                message_id: None,
            },
        },
    )
    .await;
    let reply = read_reply(&mut lines, 5).await;
    assert!(matches!(reply, Reply::Error { .. }));

    // 6. Shutdown force: true -> Ok
    send_env(
        &mut writer,
        &Envelope {
            id: 6,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    let reply = read_reply(&mut lines, 6).await;
    assert_eq!(reply, Reply::Ok);

    let server_res = tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
        .await
        .expect("server shutdown timeout")
        .expect("join error");
    assert!(server_res.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_request_does_not_block_next_session_requests_and_is_cancelled_on_detach() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("slow-request.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let marketplace = Arc::new(Marketplace::new(
        Arc::new(BlockingMarketplaceFetcher {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
        paths.cache_dir.join("marketplace"),
        paths.cache_dir.join("plugins"),
        pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
    ));
    let core = build_test_core_with_marketplace(&tmp, Arc::clone(&config), marketplace).await;
    let server_task =
        start_test_daemon(&socket, paths, Arc::clone(&config), Arc::clone(&core)).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    initialize_client(
        &mut writer,
        &mut lines,
        ClientId::new("slow-request-client"),
    )
    .await;
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut lines, 2).await,
        Reply::Attached(_)
    ));

    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::BrowseMarketplace {
                source: "https://slow.test/marketplace.json".into(),
                query: String::new(),
            },
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("marketplace request did not start");

    send_env(
        &mut writer,
        &Envelope {
            id: 4,
            req: Request::UserMessage {
                text: "must not wait for marketplace".into(),
                images: Vec::new(),
                message_id: None,
            },
        },
    )
    .await;
    send_env(
        &mut writer,
        &Envelope {
            id: 5,
            req: Request::Interrupt,
        },
    )
    .await;

    let mut prompt_reply = false;
    let mut interrupt_reply = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match read_msg(&mut lines).await {
                ServerMessage::Reply { id: 3, .. } => {
                    panic!("slow request completed before release")
                }
                ServerMessage::Reply {
                    id: 4,
                    reply: Reply::Ok,
                } => prompt_reply = true,
                ServerMessage::Reply {
                    id: 5,
                    reply: Reply::Ok,
                } => interrupt_reply = true,
                _ => {}
            }
            if prompt_reply && interrupt_reply {
                break;
            }
        }
    })
    .await
    .expect("next session request was blocked by slow request");
    assert!(prompt_reply && interrupt_reply);

    send_env(
        &mut writer,
        &Envelope {
            id: 6,
            req: Request::Detach,
        },
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), read_reply(&mut lines, 6))
            .await
            .expect("detach was blocked by slow request"),
        Reply::Ok
    );

    let cancelled = tokio::time::timeout(Duration::from_secs(2), read_reply(&mut lines, 3))
        .await
        .expect("cancelled slow request did not finish");
    assert!(matches!(
        cancelled,
        Reply::Error { message } if message.contains("cancelled")
    ));
    send_env(
        &mut writer,
        &Envelope {
            id: 7,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 7).await, Reply::Ok);
    let result = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("daemon did not stop")
        .expect("daemon task join failed");
    assert!(result.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_mcp_requests_in_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_mcp.sock");

    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let opts = DaemonOptions {
        paths,
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };

    let server_task = tokio::spawn(run(opts, core));

    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // 1. Hello
    let hello_req = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: ClientId::generate(),
            app_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    send_env(&mut writer, &hello_req).await;
    let reply = read_reply(&mut lines, 1).await;
    assert!(matches!(reply, Reply::Hello { .. }));

    // 2. ListMcpServers with no servers configured -> Reply::McpServers with empty vec
    let list_req = Envelope {
        id: 2,
        req: Request::ListMcpServers,
    };
    send_env(&mut writer, &list_req).await;
    let reply = read_reply(&mut lines, 2).await;
    match reply {
        Reply::McpServers { servers } => {
            assert!(servers.is_empty(), "expected 0 servers configured");
        }
        other => panic!("expected Reply::McpServers, got {other:?}"),
    }

    // 3. SetMcpServerEnabled on an unknown server -> Reply::Error
    let set_req = Envelope {
        id: 3,
        req: Request::SetMcpServerEnabled {
            server: "unknown_server".to_string(),
            enabled: true,
        },
    };
    send_env(&mut writer, &set_req).await;
    let reply = read_reply(&mut lines, 3).await;
    match reply {
        Reply::Error { message } => {
            assert!(
                !message.is_empty(),
                "expected error message for unknown server"
            );
        }
        other => panic!("expected Reply::Error, got {other:?}"),
    }

    // 4. Shutdown
    let shut_req = Envelope {
        id: 4,
        req: Request::Shutdown { force: true },
    };
    send_env(&mut writer, &shut_req).await;
    let reply = read_reply(&mut lines, 4).await;
    assert_eq!(reply, Reply::Ok);

    let _ = server_task.await;
}

/// `Event::SessionStatus` rides the daemon-wide stream: a client attached to
/// one session sees another session's slot badge change over the same socket.
#[tokio::test(flavor = "multi_thread")]
async fn session_status_events_reach_client_attached_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("daemon_status.sock");

    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let opts = DaemonOptions {
        paths,
        config: Arc::clone(&config),
        socket: sock_path.clone(),
        app_version: "0.1.0".to_string(),
    };

    let server_task = tokio::spawn(run(opts, Arc::clone(&core)));

    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Client attached to session A through the socket.
    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    send_env(
        &mut writer,
        &Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: ClientId::generate(),
                app_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION,
            }),
        },
    )
    .await;
    let reply = read_reply(&mut lines, 1).await;
    assert!(matches!(reply, Reply::Hello { .. }));

    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    let session_a = match read_reply(&mut lines, 2).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached, got {other:?}"),
    };

    // Session B exists only daemon-side — no client ever attaches to it.
    let session_b = core
        .open_session(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: None,
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    assert_ne!(session_a, session_b);

    core.handle(
        &session_b,
        Request::UserMessage {
            text: "hi".to_string(),
            images: Vec::new(),
            message_id: None,
        },
    )
    .await;

    // The socket sees B's badge flip Working -> Done even though the client is
    // attached to A.
    let deadline = now_ms() + 10_000;
    let mut statuses = Vec::new();
    while now_ms() < deadline {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .expect("timed out waiting for SessionStatus::Done")
            .expect("stream ended")
            .expect("read error");
        let msg: ServerMessage = serde_json::from_str(&msg).unwrap();
        if let ServerMessage::Event {
            event: Event::SessionStatus { session, status },
            ..
        } = msg
            && session == session_b
        {
            statuses.push(status);
            if status == SessionStatusKind::Done {
                break;
            }
        }
    }
    assert_eq!(
        statuses,
        vec![SessionStatusKind::Working, SessionStatusKind::Done],
        "B's badge must run Working -> Done on the global stream"
    );

    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    let reply = read_reply(&mut lines, 3).await;
    assert_eq!(reply, Reply::Ok);
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_idempotency_survives_restart_and_rejects_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("attach-restart.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let client_id = ClientId::new("restart-client");
    let key = "restart-key";
    let first_cwd = tmp.path().join("first");
    let other_cwd = tmp.path().join("other");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&other_cwd).unwrap();
    let attach = Attach::New {
        cwd: first_cwd.clone(),
        model: None,
        effort: None,
        mode: None,
    };

    let first_core = build_test_core(&tmp, Arc::clone(&config)).await;
    let first_task =
        start_test_daemon(&socket, paths.clone(), Arc::clone(&config), first_core).await;
    let first_stream = UnixStream::connect(&socket).await.unwrap();
    let (first_reader, mut first_writer) = first_stream.into_split();
    let mut first_lines = BufReader::new(first_reader).lines();
    initialize_client(&mut first_writer, &mut first_lines, client_id.clone()).await;
    send_attach(
        &mut first_writer,
        &AttachEnvelope::new(2, attach.clone(), key),
    )
    .await;
    let first_session = match read_reply(&mut first_lines, 2).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected attached session, got {other:?}"),
    };
    send_env(
        &mut first_writer,
        &Envelope {
            id: 3,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut first_lines, 3).await, Reply::Ok);
    let first_result = tokio::time::timeout(std::time::Duration::from_secs(5), first_task)
        .await
        .expect("first daemon did not stop")
        .expect("first daemon join failed");
    assert!(first_result.is_ok());
    drop(first_writer);
    drop(first_lines);

    let second_core = build_test_core(&tmp, Arc::clone(&config)).await;
    let second_task =
        start_test_daemon(&socket, paths, Arc::clone(&config), second_core.clone()).await;
    let second_stream = UnixStream::connect(&socket).await.unwrap();
    let (second_reader, mut second_writer) = second_stream.into_split();
    let mut second_lines = BufReader::new(second_reader).lines();
    initialize_client(&mut second_writer, &mut second_lines, client_id).await;

    for (index, conflict) in [
        Attach::New {
            cwd: other_cwd.clone(),
            model: None,
            effort: None,
            mode: None,
        },
        Attach::New {
            cwd: first_cwd.clone(),
            model: Some(ModelRoute::new("mock", "other-model")),
            effort: None,
            mode: None,
        },
        Attach::New {
            cwd: first_cwd.clone(),
            model: None,
            effort: None,
            mode: Some(Mode::Auto),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let id = 10 + index as u64;
        send_attach(&mut second_writer, &AttachEnvelope::new(id, conflict, key)).await;
        match read_reply(&mut second_lines, id).await {
            Reply::Error { message } => assert!(message.contains("different attach payload")),
            other => panic!("expected idempotency conflict, got {other:?}"),
        }
    }

    send_attach(
        &mut second_writer,
        &AttachEnvelope::new(20, attach.clone(), key),
    )
    .await;
    match read_reply(&mut second_lines, 20).await {
        Reply::Attached(snapshot) => assert_eq!(snapshot.meta.id, first_session),
        other => panic!("expected recorded session after restart, got {other:?}"),
    }
    assert_eq!(second_core.session_ids(), vec![first_session.clone()]);

    send_env(
        &mut second_writer,
        &Envelope {
            id: 21,
            req: Request::Attach(attach),
        },
    )
    .await;
    let no_key_session = match read_reply(&mut second_lines, 21).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected no-key attach to succeed, got {other:?}"),
    };
    assert_ne!(no_key_session, first_session);
    wait_for_session_unload(&second_core, &first_session).await;
    assert_eq!(second_core.session_ids(), vec![no_key_session.clone()]);

    send_env(
        &mut second_writer,
        &Envelope {
            id: 22,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut second_lines, 22).await, Reply::Ok);
    let second_result = tokio::time::timeout(std::time::Duration::from_secs(5), second_task)
        .await
        .expect("second daemon did not stop")
        .expect("second daemon join failed");
    assert!(second_result.is_ok());
}

/// A peer that can no longer receive anything must not leave the daemon holding
/// a connection: the write half of the socket is still open, so only the writer
/// failure can end it.
#[tokio::test(flavor = "multi_thread")]
async fn writer_failure_ends_the_connection_and_frees_its_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;

    let control = ServerControl {
        shutdown: CancellationToken::new(),
        graceful_shutdown: CancellationToken::new(),
        connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        app_version: "0.1.0".to_string(),
        pid: std::process::id(),
        paths,
        disconnected: Arc::new(tokio::sync::Notify::new()),
        attach_store: Store::open_in_memory().unwrap(),
    };
    let (daemon_end, client_end) = UnixStream::pair().unwrap();
    let client_fd = client_end.as_raw_fd();
    let connection = tokio::spawn(serve_connection(
        daemon_end,
        Arc::clone(&core),
        control.clone(),
    ));

    let (reader, mut writer) = client_end.into_split();
    let mut lines = BufReader::new(reader).lines();
    send_env(
        &mut writer,
        &Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: ClientId::generate(),
                app_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION,
            }),
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut lines, 1).await,
        Reply::Hello { .. }
    ));
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut lines, 2).await,
        Reply::Attached(_)
    ));

    // SAFETY: shuts down the read direction of our own socket end; the fd stays
    // open and owned by `writer`/`lines`.
    unsafe { libc::shutdown(client_fd, libc::SHUT_RD) };

    // The client sends nothing from here on, so the only thing the daemon can
    // learn from is its own write of a session event failing.
    core.broadcast_event(Event::Toast {
        level: pacode_types::state::ToastLevel::Info,
        title: "unreadable".to_string(),
        detail: None,
    });

    tokio::time::timeout(Duration::from_secs(5), connection)
        .await
        .expect("connection outlived its failed writer")
        .expect("connection task panicked");
    assert_eq!(
        control
            .connections
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "connection slot was not released"
    );
}

/// `Shutdown{force:false}` is answered and the daemon then exits as soon as the
/// core is idle — without waiting for the requesting client to disconnect.
#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_exits_while_the_client_stays_connected() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("graceful-shutdown.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    initialize_client(&mut writer, &mut lines, ClientId::generate()).await;
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Shutdown { force: false },
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 2).await, Reply::Ok);

    let result = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("daemon stayed alive with a connected graceful client")
        .expect("daemon task join failed");
    assert!(result.is_ok());
    assert!(!socket.exists());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), lines.next_line())
            .await
            .expect("client kept its connection after graceful shutdown")
            .expect("client connection read failed")
            .is_none()
    );
}

/// The shutdown reply must survive the shutdown it triggers. The daemon's own
/// graceful path cancels `control.shutdown` while the reply is still being
/// flushed, so a writer that aborts on that token (or drops what is already
/// queued) loses the line the client is blocked on. One handshake rarely hits
/// the window, so this repeats it.
#[tokio::test(flavor = "multi_thread")]
async fn the_shutdown_reply_is_never_lost_to_the_shutdown_it_triggers() {
    for round in 0..12 {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join(format!("shutdown-race-{round}.sock"));
        let paths = pacode_config::Paths::under(tmp.path());
        let mut config = Config::default();
        config.daemon.idle_timeout_secs = 3600;
        config
            .providers
            .insert("mock".to_string(), pacode_types::ProviderConfig::default());
        config.provider.default = Some("mock/mock-model".to_string());
        let config = Arc::new(config);
        let core = build_test_core(&tmp, Arc::clone(&config)).await;
        let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

        let stream = UnixStream::connect(&socket).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        initialize_client(&mut writer, &mut lines, ClientId::generate()).await;
        send_env(
            &mut writer,
            &Envelope {
                id: 2,
                req: Request::Shutdown { force: false },
            },
        )
        .await;

        let reply = tokio::time::timeout(Duration::from_secs(5), read_reply(&mut lines, 2))
            .await
            .unwrap_or_else(|_| panic!("round {round}: shutdown reply never arrived"));
        assert_eq!(reply, Reply::Ok, "round {round}");

        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .unwrap_or_else(|_| panic!("round {round}: daemon did not exit"))
            .expect("daemon task join failed")
            .expect("daemon returned an error");
    }
}

/// `RuntimeStats` is a daemon-level reading: it must answer on a connection
/// that never attached, and it must reflect work the daemon actually did. The
/// counts are the point — a report of zeros would look plausible and be a lie.
#[tokio::test(flavor = "multi_thread")]
async fn runtime_stats_answer_without_an_attach_and_count_real_work() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("stats.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let server_task = start_test_daemon(&socket, paths, Arc::clone(&config), core).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    initialize_client(&mut writer, &mut lines, ClientId::generate()).await;

    // No Attach: this must still be answered.
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::RuntimeStats,
        },
    )
    .await;

    let (core_stats, provider_stats) = match read_reply(&mut lines, 2).await {
        Reply::RuntimeStats { stats } => (stats.core, stats.providers),
        other => panic!("expected RuntimeStats, got {other:?}"),
    };

    // The daemon has opened and migrated its store, so the writer has done
    // work; a reading of zeros would mean the instrumentation is not wired to
    // the path that actually runs.
    assert!(
        core_stats.store.completed > 0,
        "the store writer has run by now, got {:?}",
        core_stats.store
    );
    assert!(core_stats.store.exec.count > 0, "exec must be sampled");
    assert_eq!(core_stats.sessions_resident, 0, "nothing attached yet");
    assert!(
        provider_stats.wait_total_micros == 0 && provider_stats.refused == 0,
        "a mock session made no provider calls: {provider_stats:?}"
    );

    // The ceiling the daemon applied is the configured one, not a stale default.
    assert_eq!(
        pacode_provider::provider_concurrency_ceiling(),
        config.limits.max_provider_concurrency as usize
    );

    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    let _ = read_reply(&mut lines, 3).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), server_task).await;
}

/// Detaching releases the attachment lease: work bound to the old attachment is
/// fenced, and the next attach is a fresh lease on its own session.
#[tokio::test(flavor = "multi_thread")]
async fn detach_releases_the_lease_and_fences_the_old_attachment() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("detach-lease.sock");
    let paths = pacode_config::Paths::under(tmp.path());
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = 3600;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    let config = Arc::new(config);
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let marketplace = Arc::new(Marketplace::new(
        Arc::new(BlockingMarketplaceFetcher {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
        paths.cache_dir.join("marketplace"),
        paths.cache_dir.join("plugins"),
        pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
    ));
    let core = build_test_core_with_marketplace(&tmp, Arc::clone(&config), marketplace).await;
    let server_task =
        start_test_daemon(&socket, paths, Arc::clone(&config), Arc::clone(&core)).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    initialize_client(
        &mut writer,
        &mut lines,
        ClientId::new("detach-lease-client"),
    )
    .await;
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    let first_session = match read_reply(&mut lines, 2).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached, got {other:?}"),
    };

    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::BrowseMarketplace {
                source: "https://slow.test/marketplace.json".into(),
                query: String::new(),
            },
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("slow request did not start");

    send_env(
        &mut writer,
        &Envelope {
            id: 4,
            req: Request::Detach,
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 4).await, Reply::Ok);
    assert!(matches!(
        read_reply(&mut lines, 3).await,
        Reply::Error { message } if message.contains("cancelled")
    ));
    wait_for_session_unload(&core, &first_session).await;

    send_env(
        &mut writer,
        &Envelope {
            id: 5,
            req: Request::Attach(Attach::New {
                cwd: tmp.path().to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    let second_session = match read_reply(&mut lines, 5).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached after detach, got {other:?}"),
    };
    assert_ne!(second_session, first_session);

    send_env(
        &mut writer,
        &Envelope {
            id: 6,
            req: Request::GetSnapshot,
        },
    )
    .await;
    match read_reply(&mut lines, 6).await {
        Reply::Snapshot(snapshot) => assert_eq!(snapshot.meta.id, second_session),
        other => panic!("expected Reply::Snapshot on the new lease, got {other:?}"),
    }

    release.notify_waiters();
    send_env(
        &mut writer,
        &Envelope {
            id: 7,
            req: Request::Shutdown { force: true },
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 7).await, Reply::Ok);
    let result = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("daemon did not stop")
        .expect("daemon task join failed");
    assert!(result.is_ok());
}

type ClientWriter = tokio::net::unix::OwnedWriteHalf;
type ClientLines = Lines<BufReader<tokio::net::unix::OwnedReadHalf>>;

fn test_control(paths: pacode_config::Paths) -> ServerControl {
    ServerControl {
        shutdown: CancellationToken::new(),
        graceful_shutdown: CancellationToken::new(),
        connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        app_version: "0.1.0".to_string(),
        pid: std::process::id(),
        paths,
        disconnected: Arc::new(tokio::sync::Notify::new()),
        attach_store: Store::open_in_memory().unwrap(),
    }
}

fn test_config(idle_timeout_secs: u64) -> Arc<Config> {
    let mut config = Config::default();
    config.daemon.idle_timeout_secs = idle_timeout_secs;
    config
        .providers
        .insert("mock".to_string(), pacode_types::ProviderConfig::default());
    config.provider.default = Some("mock/mock-model".to_string());
    Arc::new(config)
}

async fn serve_pair(
    core: Arc<Core>,
    paths: pacode_config::Paths,
) -> (ClientWriter, ClientLines, tokio::task::JoinHandle<()>) {
    let control = test_control(paths);
    let (daemon_end, client_end) = UnixStream::pair().unwrap();
    let connection = tokio::spawn(serve_connection(daemon_end, core, control));
    let (reader, writer) = client_end.into_split();
    (writer, BufReader::new(reader).lines(), connection)
}

async fn attach_new(
    writer: &mut ClientWriter,
    lines: &mut ClientLines,
    id: u64,
    cwd: &Path,
) -> SessionId {
    send_env(
        writer,
        &Envelope {
            id,
            req: Request::Attach(Attach::New {
                cwd: cwd.to_path_buf(),
                model: None,
                effort: None,
                mode: None,
            }),
        },
    )
    .await;
    match read_reply(lines, id).await {
        Reply::Attached(snapshot) => snapshot.meta.id,
        other => panic!("expected Reply::Attached, got {other:?}"),
    }
}

async fn send_detach(writer: &mut ClientWriter, lines: &mut ClientLines, id: u64) {
    send_env(
        writer,
        &Envelope {
            id,
            req: Request::Detach,
        },
    )
    .await;
    assert_eq!(read_reply(lines, id).await, Reply::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_session_with_a_running_turn_stays_until_the_turn_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pacode_config::Paths::under(tmp.path());
    let config = test_config(3600);
    let mock = Arc::new(MockProvider::new("mock"));
    mock.push(MockResponse::Slow {
        delay: Duration::from_secs(2),
        text: "slow answer".to_string(),
    });
    let marketplace = Arc::new(Marketplace::new(
        Arc::new(HttpFetcher::new()),
        paths.cache_dir.join("marketplace"),
        paths.cache_dir.join("plugins"),
        pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
    ));
    let core =
        build_test_core_with_marketplace_and_mock(&tmp, Arc::clone(&config), marketplace, mock)
            .await;
    let (mut writer, mut lines, connection) = serve_pair(Arc::clone(&core), paths.clone()).await;
    initialize_client(&mut writer, &mut lines, ClientId::new("live-turn")).await;
    let session_id = attach_new(&mut writer, &mut lines, 2, tmp.path()).await;

    send_env(
        &mut writer,
        &Envelope {
            id: 3,
            req: Request::UserMessage {
                text: "work".to_string(),
                images: Vec::new(),
                message_id: None,
            },
        },
    )
    .await;
    assert_eq!(read_reply(&mut lines, 3).await, Reply::Ok);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let running = core
                .session(&session_id)
                .and_then(|session| session.main_agent())
                .is_some_and(|agent| agent.is_running());
            if running {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn never started");

    send_detach(&mut writer, &mut lines, 4).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        core.session(&session_id).is_some(),
        "a running turn must keep its session resident after the detach"
    );

    wait_for_session_unload(&core, &session_id).await;
    connection.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_survives_until_its_last_attachment_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pacode_config::Paths::under(tmp.path());
    let config = test_config(3600);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let (mut first_writer, mut first_lines, first_connection) =
        serve_pair(Arc::clone(&core), paths.clone()).await;
    initialize_client(
        &mut first_writer,
        &mut first_lines,
        ClientId::new("lease-first"),
    )
    .await;
    let session_id = attach_new(&mut first_writer, &mut first_lines, 2, tmp.path()).await;

    let (mut second_writer, mut second_lines, second_connection) =
        serve_pair(Arc::clone(&core), paths).await;
    initialize_client(
        &mut second_writer,
        &mut second_lines,
        ClientId::new("lease-second"),
    )
    .await;
    send_env(
        &mut second_writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::Resume {
                session: session_id.clone(),
            }),
        },
    )
    .await;
    assert!(matches!(
        read_reply(&mut second_lines, 2).await,
        Reply::Attached(_)
    ));

    send_detach(&mut first_writer, &mut first_lines, 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(core.session(&session_id).is_some());

    send_detach(&mut second_writer, &mut second_lines, 3).await;
    wait_for_session_unload(&core, &session_id).await;
    first_connection.abort();
    second_connection.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnect_releases_the_lease_and_a_reattach_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = pacode_config::Paths::under(tmp.path());
    let config = test_config(3600);
    let core = build_test_core(&tmp, Arc::clone(&config)).await;
    let (mut writer, mut lines, connection) = serve_pair(Arc::clone(&core), paths.clone()).await;
    initialize_client(&mut writer, &mut lines, ClientId::new("disconnect")).await;
    let session_id = attach_new(&mut writer, &mut lines, 2, tmp.path()).await;

    drop(writer);
    drop(lines);
    tokio::time::timeout(Duration::from_secs(5), connection)
        .await
        .expect("the connection outlived its socket")
        .expect("connection task panicked");
    wait_for_session_unload(&core, &session_id).await;

    let (mut writer, mut lines, connection) = serve_pair(Arc::clone(&core), paths).await;
    initialize_client(&mut writer, &mut lines, ClientId::new("disconnect")).await;
    send_env(
        &mut writer,
        &Envelope {
            id: 2,
            req: Request::Attach(Attach::Resume {
                session: session_id.clone(),
            }),
        },
    )
    .await;
    match read_reply(&mut lines, 2).await {
        Reply::Attached(snapshot) => assert_eq!(snapshot.meta.id, session_id),
        other => panic!("expected the resumed session, got {other:?}"),
    }
    connection.abort();
}
