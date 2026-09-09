//! Daemon lifecycle helpers used by `codeapp daemon status|stop` and by autospawn.

use std::path::{Path, PathBuf};

use codeapp_types::ids::ClientId;
use codeapp_types::{ClientHello, Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::ClientError;

#[derive(Clone, Debug, PartialEq)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub socket: PathBuf,
}

/// Spawn `<exe> serve --detach [--socket <path>]` fully detached (`setsid`, stdio to
/// `log_file`), return the child pid. Does not wait for the socket.
pub fn spawn_daemon(
    exe: &Path,
    socket: &Path,
    log_file: &Path,
    extra_args: &[String],
) -> Result<u32, ClientError> {
    if let Some(parent) = log_file.parent() {
        let p = parent.display();
        std::fs::create_dir_all(parent)
            .map_err(|e| ClientError::Spawn(format!("failed to create log dir {p}: {e}")))?;
    }

    let p = log_file.display();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| ClientError::Spawn(format!("failed to open log file {p}: {e}")))?;

    let log_err = log
        .try_clone()
        .map_err(|e| ClientError::Spawn(format!("failed to clone log file handle: {e}")))?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve");
    cmd.arg("--socket");
    cmd.arg(socket);
    cmd.args(extra_args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(log);
    cmd.stderr(log_err);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let exe_p = exe.display();
    let child = cmd
        .spawn()
        .map_err(|e| ClientError::Spawn(format!("failed to spawn daemon {exe_p}: {e}")))?;

    Ok(child.id())
}

/// Connect, `Hello`, `Ping`, close. `None` when no daemon answers.
pub async fn daemon_status(socket: &Path, app_version: &str) -> Option<DaemonStatus> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let stream = UnixStream::connect(socket).await.ok()?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        let hello = Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: ClientId::generate(),
                app_version: app_version.to_string(),
                protocol: PROTOCOL_VERSION,
            }),
        };
        let mut line = serde_json::to_string(&hello).ok()?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await.ok()?;
        write_half.flush().await.ok()?;

        let mut reply_line = String::new();
        reader.read_line(&mut reply_line).await.ok()?;
        let msg: ServerMessage = serde_json::from_str(reply_line.trim()).ok()?;
        let (daemon_version, protocol, pid) = match msg {
            ServerMessage::Reply {
                id: 1,
                reply:
                    Reply::Hello {
                        daemon_version,
                        protocol,
                        pid,
                    },
            } => (daemon_version, protocol, pid),
            _ => return None,
        };

        let ping = Envelope {
            id: 2,
            req: Request::Ping,
        };
        let mut line = serde_json::to_string(&ping).ok()?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await.ok()?;
        write_half.flush().await.ok()?;

        reply_line.clear();
        reader.read_line(&mut reply_line).await.ok()?;
        let msg: ServerMessage = serde_json::from_str(reply_line.trim()).ok()?;
        match msg {
            ServerMessage::Reply {
                id: 2,
                reply: Reply::Pong,
            } => Some(DaemonStatus {
                pid,
                version: daemon_version,
                protocol,
                socket: socket.to_path_buf(),
            }),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Send `Shutdown{force}`. Ok when the daemon acknowledged (it exits when idle, or
/// immediately with `force`).
pub async fn stop_daemon(socket: &Path, app_version: &str, force: bool) -> Result<(), ClientError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| ClientError::Connect {
            path: socket.to_path_buf(),
            source: e,
        })?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let hello = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: ClientId::generate(),
            app_version: app_version.to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    let mut line = serde_json::to_string(&hello)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reply_line = String::new();
    reader.read_line(&mut reply_line).await?;
    let msg: ServerMessage = serde_json::from_str(reply_line.trim())?;
    match msg {
        ServerMessage::Reply {
            id: 1,
            reply: Reply::Hello { protocol, .. },
        } => {
            if protocol != PROTOCOL_VERSION {
                return Err(ClientError::Protocol {
                    daemon: protocol,
                    client: PROTOCOL_VERSION,
                });
            }
        }
        ServerMessage::Reply {
            id: 1,
            reply: Reply::Error { message },
        } => return Err(ClientError::Daemon(message)),
        other => {
            return Err(ClientError::UnexpectedReply {
                request: "hello".into(),
                reply: format!("{other:?}"),
            });
        }
    }

    let shutdown = Envelope {
        id: 2,
        req: Request::Shutdown { force },
    };
    let mut line = serde_json::to_string(&shutdown)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    reply_line.clear();
    reader.read_line(&mut reply_line).await?;
    let msg: ServerMessage = serde_json::from_str(reply_line.trim())?;
    match msg {
        ServerMessage::Reply {
            id: 2,
            reply: Reply::Ok,
        } => Ok(()),
        ServerMessage::Reply {
            id: 2,
            reply: Reply::Error { message },
        } => Err(ClientError::Daemon(message)),
        other => Err(ClientError::UnexpectedReply {
            request: "shutdown".into(),
            reply: format!("{other:?}"),
        }),
    }
}

/// Poll until the socket accepts a connection or `timeout` elapses.
pub async fn wait_for_socket(socket: &Path, timeout: std::time::Duration) -> bool {
    let start = tokio::time::Instant::now();
    let poll_interval = std::time::Duration::from_millis(10);
    loop {
        if UnixStream::connect(socket).await.is_ok() {
            return true;
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return false;
        }
        let remaining = timeout - elapsed;
        tokio::time::sleep(poll_interval.min(remaining)).await;
    }
}
