use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pacode_types::ids::ClientId;
use pacode_types::{
    Attach, ClientHello, Envelope, PROTOCOL_VERSION, Reply, Request, ServerMessage, SessionId,
    SessionSnapshot,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{ClientEvent, ClientOptions};

pub(crate) fn spawn_writer(
    mut write_half: OwnedWriteHalf,
    mut rx: mpsc::Receiver<String>,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                        Some(line) => {
                            if write_half.write_all(line.as_bytes()).await.is_err() {
                                break;
                            }
                            if write_half.flush().await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    })
}

pub(crate) fn emit_event(
    events_tx: &mpsc::Sender<ClientEvent>,
    dropped_events: &AtomicUsize,
    event: ClientEvent,
) {
    match events_tx.try_send(event) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(ev)) => {
            let count = dropped_events.fetch_add(1, Ordering::Relaxed) + 1;
            log::warn!("event queue full (capacity 1024), dropped event count={count}: {ev:?}");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            log::debug!("event receiver closed");
        }
    }
}

pub(crate) fn fail_pending(pending: &Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Reply>>>>) {
    if let Ok(mut map) = pending.lock() {
        map.clear();
    }
}

async fn run_reader(
    reader: &mut BufReader<OwnedReadHalf>,
    pending: &Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    events_tx: &mpsc::Sender<ClientEvent>,
    dropped_events: &AtomicUsize,
    cancel_token: &CancellationToken,
) -> String {
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            _ = cancel_token.cancelled() => {
                return "cancelled".to_string();
            }
            res = reader.read_line(&mut line) => {
                match res {
                    Ok(0) => {
                        return "server closed connection".to_string();
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<ServerMessage>(trimmed) {
                            Ok(ServerMessage::Reply { id, reply }) => {
                                if let Ok(mut p) = pending.lock()
                                    && let Some(tx) = p.remove(&id)
                                {
                                    let _ = tx.send(reply);
                                }
                            }
                            Ok(ServerMessage::Event { seq, event }) => {
                                emit_event(events_tx, dropped_events, ClientEvent::Event { seq, event });
                            }
                            Err(e) => {
                                log::error!(
                                    "protocol error: failed to deserialize ServerMessage: {e}: {trimmed}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        return format!("read error: {e}");
                    }
                }
            }
        }
    }
}

fn next_backoff(cur: std::time::Duration) -> std::time::Duration {
    match cur.as_millis() {
        500 => std::time::Duration::from_secs(1),
        1000 => std::time::Duration::from_secs(2),
        2000 => std::time::Duration::from_secs(4),
        _ => std::time::Duration::from_secs(8),
    }
}

async fn do_reconnect_hello(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    client_id: &ClientId,
    app_version: &str,
    cancel_token: &CancellationToken,
) -> Result<(String, u32, bool), ()> {
    let hello = Envelope {
        id: 1,
        req: Request::Hello(ClientHello {
            client_id: client_id.clone(),
            app_version: app_version.to_string(),
            protocol: PROTOCOL_VERSION,
        }),
    };
    let mut line = serde_json::to_string(&hello).map_err(|_| ())?;
    line.push('\n');

    tokio::select! {
        _ = cancel_token.cancelled() => return Err(()),
        res = write_half.write_all(line.as_bytes()) => res.map_err(|_| ())?,
    }
    write_half.flush().await.map_err(|_| ())?;

    let mut reply_line = String::new();
    tokio::select! {
        _ = cancel_token.cancelled() => return Err(()),
        res = reader.read_line(&mut reply_line) => {
            if res.map_err(|_| ())? == 0 {
                return Err(());
            }
        }
    }

    let msg: ServerMessage = serde_json::from_str(reply_line.trim()).map_err(|_| ())?;
    match msg {
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
                return Err(());
            }
            let version_mismatch = daemon_version != app_version;
            Ok((daemon_version, pid, version_mismatch))
        }
        _ => Err(()),
    }
}

enum ReconnectAttachOutcome {
    Attached(Box<SessionSnapshot>),
    Error(String),
}

async fn do_reconnect_attach(
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: &SessionId,
    cancel_token: &CancellationToken,
) -> Result<ReconnectAttachOutcome, ()> {
    let attach = Envelope {
        id: 2,
        req: Request::Attach(Attach::Resume {
            session: session_id.clone(),
        }),
    };
    let mut line = serde_json::to_string(&attach).map_err(|_| ())?;
    line.push('\n');

    tokio::select! {
        _ = cancel_token.cancelled() => return Err(()),
        res = write_half.write_all(line.as_bytes()) => res.map_err(|_| ())?,
    }
    write_half.flush().await.map_err(|_| ())?;

    let mut reply_line = String::new();
    tokio::select! {
        _ = cancel_token.cancelled() => return Err(()),
        res = reader.read_line(&mut reply_line) => {
            if res.map_err(|_| ())? == 0 {
                return Err(());
            }
        }
    }

    let msg: ServerMessage = serde_json::from_str(reply_line.trim()).map_err(|_| ())?;
    match msg {
        ServerMessage::Reply {
            id: 2,
            reply: Reply::Attached(snapshot),
        } => Ok(ReconnectAttachOutcome::Attached(Box::new(snapshot))),
        ServerMessage::Reply {
            id: 2,
            reply: Reply::Error { message },
        } => Ok(ReconnectAttachOutcome::Error(message)),
        _ => Err(()),
    }
}

pub(crate) struct SupervisorCtx {
    pub reader: BufReader<OwnedReadHalf>,
    pub socket_path: PathBuf,
    pub client_id: ClientId,
    pub opts: ClientOptions,
    pub pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    pub events_tx: mpsc::Sender<ClientEvent>,
    pub dropped_events: Arc<AtomicUsize>,
    pub writer_tx_holder: Arc<std::sync::Mutex<Option<mpsc::Sender<String>>>>,
    pub connected: Arc<AtomicBool>,
    pub attached_session: Arc<std::sync::Mutex<Option<SessionId>>>,
    pub cancel_token: CancellationToken,
}

pub(crate) async fn run_supervisor(mut ctx: SupervisorCtx) {
    loop {
        if ctx.cancel_token.is_cancelled() {
            break;
        }

        let reason = run_reader(
            &mut ctx.reader,
            &ctx.pending,
            &ctx.events_tx,
            &ctx.dropped_events,
            &ctx.cancel_token,
        )
        .await;

        if ctx.cancel_token.is_cancelled() {
            break;
        }

        // Connection dropped
        log::warn!("daemon connection lost: {reason}");
        ctx.connected.store(false, Ordering::Relaxed);
        if let Ok(mut w) = ctx.writer_tx_holder.lock() {
            *w = None;
        }
        fail_pending(&ctx.pending);
        emit_event(
            &ctx.events_tx,
            &ctx.dropped_events,
            ClientEvent::Disconnected { reason },
        );

        let mut attempt = 1u32;
        let mut backoff = std::time::Duration::from_millis(500);
        let mut force_8s = false;

        let next_reader = loop {
            if ctx.cancel_token.is_cancelled() {
                return;
            }

            log::info!(
                "attempting reconnect {attempt} to daemon at {}",
                ctx.socket_path.display()
            );
            emit_event(
                &ctx.events_tx,
                &ctx.dropped_events,
                ClientEvent::Reconnecting { attempt },
            );
            attempt = attempt.saturating_add(1);

            let delay = if force_8s {
                std::time::Duration::from_secs(8)
            } else {
                backoff
            };

            backoff = next_backoff(backoff);
            force_8s = false;

            tokio::select! {
                _ = ctx.cancel_token.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }

            if ctx.cancel_token.is_cancelled() {
                return;
            }

            let stream = match tokio::net::UnixStream::connect(&ctx.socket_path).await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let (read_half, mut write_half) = stream.into_split();
            let mut cur_reader = BufReader::new(read_half);

            let hello_res = do_reconnect_hello(
                &mut write_half,
                &mut cur_reader,
                &ctx.client_id,
                &ctx.opts.app_version,
                &ctx.cancel_token,
            )
            .await;

            let (daemon_version, pid, version_mismatch) = match hello_res {
                Ok(info) => info,
                Err(_) => continue,
            };

            let remembered = if let Ok(guard) = ctx.attached_session.lock() {
                guard.clone()
            } else {
                None
            };

            let snapshot_opt = if let Some(session_id) = remembered {
                let attach_res = do_reconnect_attach(
                    &mut write_half,
                    &mut cur_reader,
                    &session_id,
                    &ctx.cancel_token,
                )
                .await;

                match attach_res {
                    Ok(ReconnectAttachOutcome::Attached(snapshot)) => Some(*snapshot),
                    Ok(ReconnectAttachOutcome::Error(message)) => {
                        emit_event(
                            &ctx.events_tx,
                            &ctx.dropped_events,
                            ClientEvent::Disconnected { reason: message },
                        );
                        force_8s = true;
                        continue;
                    }
                    Err(_) => continue,
                }
            } else {
                None
            };

            let (new_writer_tx, new_writer_rx) = mpsc::channel::<String>(128);
            if let Ok(mut w) = ctx.writer_tx_holder.lock() {
                *w = Some(new_writer_tx);
            }
            spawn_writer(write_half, new_writer_rx, ctx.cancel_token.clone());
            ctx.connected.store(true, Ordering::Relaxed);
            log::info!("reconnected to daemon: pid={pid}, daemon_version={daemon_version}");

            emit_event(
                &ctx.events_tx,
                &ctx.dropped_events,
                ClientEvent::Connected {
                    daemon_version,
                    pid,
                    version_mismatch,
                },
            );

            if let Some(snapshot) = snapshot_opt {
                log::info!("reconnect attached to session {}", snapshot.meta.id);
                emit_event(
                    &ctx.events_tx,
                    &ctx.dropped_events,
                    ClientEvent::Snapshot(snapshot),
                );
            }

            break cur_reader;
        };

        ctx.reader = next_reader;
    }
}
