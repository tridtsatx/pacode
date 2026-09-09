//! `Client`: one connection with reconnect.

mod reconnect;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use codeapp_types::ids::ClientId;
use codeapp_types::{
    Attach, ClientHello, Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage, SessionId,
    SessionSnapshot,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{ClientError, ClientEvent, ClientExt, ClientOptions, EventReceiver};

pub struct Client {
    opts: ClientOptions,
    client_id: ClientId,
    next_id: Arc<AtomicU64>,
    pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    writer_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<String>>>>,
    connected: Arc<AtomicBool>,
    attached_session: Arc<std::sync::Mutex<Option<SessionId>>>,
    cancel_token: CancellationToken,
    supervisor_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Client {
    /// Connect (spawning the daemon when allowed), send `Hello`, start the reader.
    /// The returned receiver yields `ClientEvent`s (the first is `Connected`).
    pub async fn connect(opts: ClientOptions) -> Result<(Client, EventReceiver), ClientError> {
        let socket_path = opts.socket_path();
        let stream = match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(s) => s,
            Err(connect_err) => {
                if opts.spawn_daemon {
                    let exe = match &opts.exe {
                        Some(e) => e.clone(),
                        None => std::env::current_exe()
                            .map_err(|e| ClientError::Spawn(e.to_string()))?,
                    };
                    crate::daemon_ctl::spawn_daemon(
                        &exe,
                        &socket_path,
                        &opts.paths.daemon_log(),
                        &[],
                    )?;
                    if !crate::daemon_ctl::wait_for_socket(
                        &socket_path,
                        std::time::Duration::from_secs(3),
                    )
                    .await
                    {
                        return Err(ClientError::Connect {
                            path: socket_path,
                            source: std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "timed out waiting for daemon socket",
                            ),
                        });
                    }
                    tokio::net::UnixStream::connect(&socket_path)
                        .await
                        .map_err(|e| ClientError::Connect {
                            path: socket_path.clone(),
                            source: e,
                        })?
                } else {
                    return Err(ClientError::Connect {
                        path: socket_path,
                        source: connect_err,
                    });
                }
            }
        };

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let client_id = ClientId::generate();

        let hello_env = Envelope {
            id: 1,
            req: Request::Hello(ClientHello {
                client_id: client_id.clone(),
                app_version: opts.app_version.clone(),
                protocol: PROTOCOL_VERSION,
            }),
        };
        let mut hello_line = serde_json::to_string(&hello_env)?;
        hello_line.push('\n');
        write_half.write_all(hello_line.as_bytes()).await?;
        write_half.flush().await?;

        let mut reply_line = String::new();
        reader.read_line(&mut reply_line).await?;
        let server_msg: ServerMessage = serde_json::from_str(reply_line.trim())?;
        let (daemon_version, pid, version_mismatch) = match server_msg {
            ServerMessage::Reply {
                id: 1,
                reply:
                    Reply::Hello {
                        daemon_version,
                        protocol,
                        pid,
                    },
            } => {
                if protocol != PROTOCOL_VERSION {
                    return Err(ClientError::Protocol {
                        daemon: protocol,
                        client: PROTOCOL_VERSION,
                    });
                }
                let version_mismatch = daemon_version != opts.app_version;
                (daemon_version, pid, version_mismatch)
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
        };

        let (events_tx, events_rx) = mpsc::channel(1024);
        let dropped_events = Arc::new(AtomicUsize::new(0));
        reconnect::emit_event(
            &events_tx,
            &dropped_events,
            ClientEvent::Connected {
                daemon_version,
                pid,
                version_mismatch,
            },
        );

        let cancel_token = CancellationToken::new();
        let (writer_tx, writer_rx) = mpsc::channel::<String>(128);
        reconnect::spawn_writer(write_half, writer_rx, cancel_token.clone());
        let writer_tx_holder = Arc::new(std::sync::Mutex::new(Some(writer_tx)));

        let next_id = Arc::new(AtomicU64::new(2));
        let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let attached_session = Arc::new(std::sync::Mutex::new(None));

        let supervisor_handle = tokio::spawn(reconnect::run_supervisor(reconnect::SupervisorCtx {
            reader,
            socket_path,
            client_id: client_id.clone(),
            opts: opts.clone(),
            pending: pending.clone(),
            events_tx,
            dropped_events,
            writer_tx_holder: writer_tx_holder.clone(),
            connected: connected.clone(),
            attached_session: attached_session.clone(),
            cancel_token: cancel_token.clone(),
        }));

        Ok((
            Client {
                opts,
                client_id,
                next_id,
                pending,
                writer_tx: writer_tx_holder,
                connected,
                attached_session,
                cancel_token,
                supervisor_handle: Some(supervisor_handle),
            },
            events_rx,
        ))
    }

    pub fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// Attach to a session and return its snapshot; remembered for reconnects.
    pub async fn attach(&self, attach: Attach) -> Result<SessionSnapshot, ClientError> {
        match self.request(Request::Attach(attach)).await? {
            Reply::Attached(snapshot) => {
                if let Ok(mut guard) = self.attached_session.lock() {
                    *guard = Some(snapshot.meta.id.clone());
                }
                Ok(snapshot)
            }
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "attach".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    pub fn session(&self) -> Option<SessionId> {
        self.attached_session
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Send a request and await its reply (with `request_timeout`).
    pub async fn request(&self, req: Request) -> Result<Reply, ClientError> {
        if !self.is_connected() {
            return Err(ClientError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().map_err(|_| ClientError::Disconnected)?;
            pending.insert(id, tx);
        }

        let envelope = Envelope { id, req };
        let mut line = match serde_json::to_string(&envelope) {
            Ok(l) => l,
            Err(e) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                return Err(ClientError::Json(e));
            }
        };
        line.push('\n');

        let writer = {
            let guard = self
                .writer_tx
                .lock()
                .map_err(|_| ClientError::Disconnected)?;
            guard.clone()
        };
        let Some(writer) = writer else {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            return Err(ClientError::Disconnected);
        };

        if writer.send(line).await.is_err() {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            return Err(ClientError::Disconnected);
        }

        match tokio::time::timeout(self.opts.request_timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(ClientError::Disconnected),
            Err(_) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                Err(ClientError::Timeout)
            }
        }
    }

    /// `request` expecting `Reply::Ok`; `Reply::Error` becomes `ClientError::Daemon`.
    pub async fn ok(&self, req: Request) -> Result<(), ClientError> {
        match self.request(req).await? {
            Reply::Ok => Ok(()),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "ok".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Stop the reader/reconnect loop and close the socket.
    pub async fn close(mut self) {
        self.cancel_token.cancel();
        self.connected.store(false, Ordering::Relaxed);
        if let Ok(mut w) = self.writer_tx.lock() {
            *w = None;
        }
        reconnect::fail_pending(&self.pending);
        if let Some(handle) = self.supervisor_handle.take() {
            let _ = handle.await;
        }
    }
}

impl ClientExt for Client {
    fn attached_session(&self) -> Option<SessionId> {
        self.session()
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("client_id", &self.client_id)
            .field("connected", &self.is_connected())
            .field("session", &self.session())
            .finish()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}
