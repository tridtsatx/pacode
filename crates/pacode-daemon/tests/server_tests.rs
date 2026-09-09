//! Integration tests for pacode-daemon.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_core::{Core, CoreDeps};
use pacode_daemon::{DaemonOptions, build_core, run, server::bind_socket};
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::{MockProvider, MockResponse};
use pacode_store::Store;
use pacode_tools::builtin_tools;
use pacode_types::protocol::{
    Attach, ClientHello, Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage,
};
use pacode_types::{ClientId, Config, ExecConfig, ModelRoute};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;

async fn send_env<W: AsyncWriteExt + Unpin>(writer: &mut W, env: &Envelope) {
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

async fn build_test_core(tmp: &tempfile::TempDir, config: Arc<Config>) -> Arc<Core> {
    let paths = pacode_config::Paths::under(tmp.path());
    let store = Store::open_in_memory().unwrap();
    let tasks = TaskManager::new(tmp.path().to_path_buf(), ExecConfig::default());
    let mcp = McpPool::new(BTreeMap::new(), None, None);

    let mut providers = ProviderRegistry::empty();
    let mock = Arc::new(MockProvider::new("mock"));
    mock.push(MockResponse::Text("Reply 1".to_string()));
    mock.push(MockResponse::Text("Reply 2".to_string()));
    mock.push(MockResponse::Text("Reply 3".to_string()));
    providers.insert(mock);
    providers.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let tools = builtin_tools();

    let deps = CoreDeps {
        config,
        paths,
        providers: Arc::new(providers),
        tools,
        tasks,
        mcp,
        store,
        app_version: "0.1.0".to_string(),
    };

    Core::new(deps).await
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

    let server_task = tokio::spawn(run(opts, core));

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
    let attach_req = Envelope {
        id: 2,
        req: Request::Attach(Attach::New {
            cwd: tmp.path().to_path_buf(),
            model: None,
            effort: None,
            mode: None,
        }),
    };
    send_env(&mut writer, &attach_req).await;

    let reply = read_reply(&mut lines, 2).await;
    match reply {
        Reply::Attached(snapshot) => {
            assert_eq!(snapshot.meta.cwd, tmp.path());
        }
        other => panic!("expected Reply::Attached, got {other:?}"),
    }

    // 3. UserMessage -> Ok, then Events including text_delta and turn_ended
    let msg_req = Envelope {
        id: 3,
        req: Request::UserMessage {
            text: "Hello agent".to_string(),
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

    // 5. Shutdown{force:true} -> Ok and server finishes and socket file is gone
    let shut_req = Envelope {
        id: 5,
        req: Request::Shutdown { force: true },
    };
    send_env(&mut writer, &shut_req).await;

    let reply = read_reply(&mut lines, 5).await;
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
